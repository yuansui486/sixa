import { call } from "./api";
type Entry = {
  id: string;
  url: string;
  bytes: number;
  pins: number;
  used: number;
};
type Request = {
  id: string;
  controller: AbortController;
  consumers: number;
  entry?: Entry;
  promise: Promise<Entry>;
};
type Waiter = {
  signal: AbortSignal;
  resolve: () => void;
  cancel: () => void;
};
const entries = new Map<string, Entry>();
const pending = new Map<string, Request>();
const generations = new Map<string, object>();
const waiting: Waiter[] = [];
const BUDGET = 96 * 1024 * 1024;
let active = 0;
function closed() {
  return Error("预览已关闭");
}
async function slot(signal: AbortSignal) {
  if (signal.aborted) throw closed();
  if (active < 2) {
    active++;
    return;
  }
  await new Promise<void>((resolve, reject) => {
    const waiter: Waiter = {
      signal,
      resolve,
      cancel: () => {
        const index = waiting.indexOf(waiter);
        if (index >= 0) waiting.splice(index, 1);
        reject(closed());
      },
    };
    waiting.push(waiter);
    signal.addEventListener("abort", waiter.cancel, { once: true });
  });
}
function releaseSlot() {
  const next = waiting.shift();
  if (next) {
    next.signal.removeEventListener("abort", next.cancel);
    next.resolve();
  } else active--;
}
function trim() {
  let bytes = [...entries.values()].reduce(
    (sum, entry) => sum + entry.bytes,
    0,
  );
  for (const [key, entry] of [...entries].sort(
    (a, b) => a[1].used - b[1].used,
  )) {
    if (bytes <= BUDGET) break;
    if (entry.pins) continue;
    URL.revokeObjectURL(entry.url);
    entries.delete(key);
    bytes -= entry.bytes;
  }
}
export function clearPreviewCache(id?: string) {
  if (id === undefined) generations.clear();
  else generations.delete(id);
  for (const [key, request] of pending) {
    if (id === undefined || request.id === id) {
      pending.delete(key);
      request.controller.abort();
    }
  }
  for (const [key, entry] of entries)
    if (id === undefined || entry.id === id) {
      URL.revokeObjectURL(entry.url);
      entries.delete(key);
    }
}
export async function acquirePage(
  id: string,
  result: boolean,
  page: number,
  revision: number,
  width: number,
  height: number,
  maxDimension = 1400,
  options: { draft?: boolean; signal?: AbortSignal } = {},
) {
  if (options.signal?.aborted) throw closed();
  const cacheRevision = result || options.draft ? revision : 0;
  const key = `${id}:${options.draft ? "draft" : result}:${cacheRevision}:${page}:${maxDimension}`;
  let generation = generations.get(id);
  if (!generation) {
    generation = {};
    generations.set(id, generation);
  }
  const valid = () => generations.get(id) === generation;
  let entry = entries.get(key);
  let request = pending.get(key);
  if (!request && !entry) {
    const controller = new AbortController();
    const requestId = crypto.randomUUID();
    const promise = (async () => {
      let reserved = false;
      const cancelNative = () => {
        void call("cancel_document_preview", { requestId }).catch(() => {});
      };
      try {
        await slot(controller.signal);
        reserved = true;
        // A cleared request must not start native rendering after leaving the queue.
        if (!valid() || controller.signal.aborted) throw closed();
        if (options.draft)
          controller.signal.addEventListener("abort", cancelNative, {
            once: true,
          });
        const args = {
          id,
          page,
          maxDimension,
          expectedRevision: revision,
        };
        const raw = options.draft
          ? await call("document_draft_page", { ...args, requestId })
          : await call("document_page", { ...args, result });
        if (!valid() || controller.signal.aborted) throw closed();
        const bytes =
          raw instanceof ArrayBuffer
            ? new Uint8Array(raw)
            : new Uint8Array(raw);
        const scale = Math.min(1, maxDimension / Math.max(width, height));
        const entry = {
          id,
          url: URL.createObjectURL(new Blob([bytes], { type: "image/png" })),
          bytes:
            bytes.byteLength + Math.ceil(width * height * scale * scale * 4),
          // Reserve awaiting consumers before publishing; another completion
          // must not evict a URL before its awaiting consumers receive it.
          pins: request!.consumers,
          used: Date.now(),
        };
        entries.set(key, entry);
        request!.entry = entry;
        return entry;
      } finally {
        controller.signal.removeEventListener("abort", cancelNative);
        if (reserved) releaseSlot();
        if (pending.get(key) === request) pending.delete(key);
      }
    })();
    request = { id, controller, consumers: 0, promise };
    pending.set(key, request);
  }
  if (entry) entry.pins++;
  else {
    const shared = request!;
    shared.consumers++;
    let abort: (() => void) | undefined;
    const cancelled = new Promise<never>((_, reject) => {
      abort = () => {
        if (shared.entry)
          shared.entry.pins = Math.max(0, shared.entry.pins - 1);
        shared.consumers--;
        if (!shared.consumers && !shared.entry) {
          if (pending.get(key) === shared) pending.delete(key);
          shared.controller.abort();
        }
        trim();
        reject(closed());
      };
      options.signal?.addEventListener("abort", abort, { once: true });
    });
    try {
      entry = await Promise.race([shared.promise, cancelled]);
    } finally {
      if (abort) options.signal?.removeEventListener("abort", abort);
    }
    if (!valid() || options.signal?.aborted) throw closed();
  }
  entry.used = Date.now();
  trim();
  let released = false;
  return {
    url: entry.url,
    release: () => {
      if (released) return;
      released = true;
      entry.pins = Math.max(0, entry.pins - 1);
      trim();
    },
  };
}
