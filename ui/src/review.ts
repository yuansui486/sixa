import { call, message, selection, type Region } from "./api";
import { useWorkbench } from "./store";

// A single queue survives route changes. Every mutation obtains its revision at
// execution time; acknowledgements never replace edits made while IPC is pending.
let tail: Promise<void> = Promise.resolve();
let failed: (() => Promise<void>) | null = null;
const deferred: (() => Promise<void>)[] = [];
let epoch = 0;
export function resetReview() {
  epoch++;
  failed = null;
  deferred.length = 0;
  tail = Promise.resolve();
  useWorkbench.setState({ saving: 0, saveError: "", pendingTaskId: null });
}
function enqueue(operation: () => Promise<void>): Promise<void> {
  const currentEpoch = epoch;
  useWorkbench.setState((s) => ({ saving: s.saving + 1 }));
  const result = tail
    .then(async () => {
      if (currentEpoch !== epoch) return;
      if (failed) {
        deferred.push(operation);
        throw Error("上次修改未保存，请先重试保存");
      }
      try {
        await operation();
        if (currentEpoch === epoch) useWorkbench.setState({ saveError: "" });
      } catch (error) {
        if (currentEpoch === epoch) {
          failed = operation;
          useWorkbench.setState({ saveError: message(error) });
        }
        throw error;
      }
    })
    .finally(() => {
      if (currentEpoch === epoch)
        useWorkbench.setState((s) => ({ saving: Math.max(0, s.saving - 1) }));
    });
  tail = result.catch(() => undefined);
  return result;
}

export function saveEntities(): Promise<void> {
  const id = useWorkbench.getState().task?.meta.id;
  const savedEpoch = epoch;
  let sent:
    | {
        selections: ReturnType<typeof selection>;
        expectedRevision: number;
        mutationId: string;
        generation: number;
      }
    | undefined;
  return enqueue(async () => {
    const state = useWorkbench.getState();
    if (!state.task || state.task.meta.id !== id || (!state.dirty && !sent))
      return;
    sent ??= {
      selections: selection(state.task.entities),
      expectedRevision: state.task.revision,
      mutationId: crypto.randomUUID(),
      generation: state.generation,
    };
    const updated = await call("review_patch", {
      id: state.task.meta.id,
      selections: sent.selections,
      expectedRevision: sent.expectedRevision,
      mutationId: sent.mutationId,
    });
    if (savedEpoch === epoch)
      useWorkbench.getState().markSaved(updated, sent.generation);
  });
}

export function saveRegion(id: string, region: Region | string): Promise<void> {
  const savedEpoch = epoch;
  const mutationId = crypto.randomUUID();
  let expectedRevision: number | undefined;
  return enqueue(async () => {
    const task = useWorkbench.getState().task;
    if (!task || task.meta.id !== id)
      throw Error("任务已切换，请重新打开原任务确认修改");
    expectedRevision ??= task.revision;
    const args = { id, expectedRevision, mutationId };
    const ack =
      typeof region === "string"
        ? await call("remove_region", { ...args, regionId: region })
        : await call("upsert_region", { ...args, region });
    if (savedEpoch === epoch && useWorkbench.getState().task?.meta.id === id) {
      useWorkbench.getState().setRevision(ack.revision);
      useWorkbench.setState((s) => ({
        task: s.task
          ? { ...s.task, meta: { ...s.task.meta, reviewed_revision: 0 } }
          : null,
      }));
    }
  });
}

export async function flushReview(): Promise<void> {
  const savedEpoch = epoch;
  await tail;
  if (savedEpoch !== epoch) return;
  if (failed)
    throw Error(useWorkbench.getState().saveError || "修改未保存，请重试保存");
  do {
    await saveEntities();
    await tail;
    if (savedEpoch !== epoch) return;
  } while (useWorkbench.getState().dirty && !failed);
  if (failed) throw Error(useWorkbench.getState().saveError);
}

export async function retryReview(): Promise<void> {
  await tail;
  const operation = failed;
  failed = null;
  if (operation) await enqueue(operation);
  while (deferred.length) await enqueue(deferred.shift()!);
  await flushReview();
}

export async function undoReview(): Promise<void> {
  const before = useWorkbench.getState().task;
  if (!before || !useWorkbench.getState().undo.length) return;
  useWorkbench.getState().undoLast();
  const after = useWorkbench.getState().task!;
  for (const region of before.regions)
    if (!after.regions.some((r) => r.id === region.id))
      void saveRegion(before.meta.id, region.id).catch(() => undefined);
  for (const region of after.regions)
    if (
      JSON.stringify(before.regions.find((r) => r.id === region.id)) !==
      JSON.stringify(region)
    )
      void saveRegion(before.meta.id, region).catch(() => undefined);
  await flushReview();
}
