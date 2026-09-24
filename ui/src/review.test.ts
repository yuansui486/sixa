import { beforeEach, describe, expect, it, vi } from "vitest";
import { call, type TaskView } from "./api";
import { useWorkbench } from "./store";
import {
  flushReview,
  resetReview,
  retryReview,
  saveEntities,
  saveRegion,
} from "./review";
vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  call: vi.fn(),
}));
const fixture = (): TaskView => ({
  meta: {
    id: "task",
    kind: "pdf",
    state: "awaiting_review",
    created_at: 0,
    updated_at: 0,
    error: null,
  },
  text: "张三",
  extension: "pdf",
  preview: null,
  entities: [
    {
      id: "person",
      entity_type: "PERSON",
      type_label: "姓名",
      score: 1,
      source: "test",
      text: "张三",
      display: { start: 0, end: 2 },
      selected: true,
      replacement: null,
    },
  ],
  regions: [],
  options: { ocr_profile: "mobile", pdf_mode: "safe_rebuild" },
  warnings: [],
  revision: 1,
});
beforeEach(() => {
  resetReview();
  vi.mocked(call).mockReset();
  useWorkbench.getState().setTask(fixture());
});
describe("review saves", () => {
  it("does not let an old session response alter the new session saving counter", async () => {
    const replies: ((view: TaskView) => void)[] = [];
    vi.mocked(call).mockImplementation(
      () =>
        new Promise((resolve) =>
          replies.push(resolve as (view: TaskView) => void),
        ),
    );
    useWorkbench
      .getState()
      .change(fixture().entities.map((e) => ({ ...e, replacement: "old" })));
    const oldSave = saveEntities();
    await vi.waitFor(() => expect(replies).toHaveLength(1));
    resetReview();
    useWorkbench.getState().setTask(fixture());
    useWorkbench
      .getState()
      .change(fixture().entities.map((e) => ({ ...e, replacement: "new" })));
    const newSave = saveEntities();
    await vi.waitFor(() => expect(replies).toHaveLength(2));
    replies[0]({ ...fixture(), revision: 2 });
    await oldSave;
    expect(useWorkbench.getState().saving).toBe(1);
    expect(useWorkbench.getState().task?.entities[0].replacement).toBe("new");
    replies[1]({ ...fixture(), revision: 2 });
    await newSave;
    expect(useWorkbench.getState().saving).toBe(0);
  });
  it("retries a region with the same mutation identity and original expected revision", async () => {
    vi.mocked(call)
      .mockRejectedValueOnce(Error("响应丢失"))
      .mockResolvedValueOnce({ revision: 2, region_id: "box" });
    await expect(saveRegion("task", "box")).rejects.toThrow("响应丢失");
    const first = vi.mocked(call).mock.calls[0][1];
    expect(first).toMatchObject({
      expectedRevision: 1,
      mutationId: expect.any(String),
    });
    await retryReview();
    expect(vi.mocked(call).mock.calls[1][1]).toEqual(first);
  });
  it("preserves a newer edit while an earlier response is in flight and flushes it before leaving", async () => {
    let release!: (task: TaskView) => void;
    vi.mocked(call).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = resolve as typeof release;
        }),
    );
    const initial = fixture();
    useWorkbench
      .getState()
      .change(initial.entities.map((e) => ({ ...e, replacement: "甲" })));
    const first = saveEntities();
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    useWorkbench
      .getState()
      .change(initial.entities.map((e) => ({ ...e, replacement: "乙" })));
    release({
      ...initial,
      entities: initial.entities.map((e) => ({ ...e, replacement: "甲" })),
      revision: 2,
    });
    await first;
    expect(useWorkbench.getState().task?.entities[0].replacement).toBe("乙");
    expect(useWorkbench.getState().dirty).toBe(true);
    vi.mocked(call).mockResolvedValueOnce({
      ...initial,
      entities: initial.entities.map((e) => ({ ...e, replacement: "乙" })),
      revision: 3,
    });
    await flushReview();
    expect(vi.mocked(call).mock.calls[1][1]).toMatchObject({
      expectedRevision: 2,
      selections: [{ id: "person", replacement: "乙", selected: true }],
    });
    expect(useWorkbench.getState().dirty).toBe(false);
  });
  it("serializes region and entity writes using the acknowledged revision", async () => {
    vi.mocked(call)
      .mockResolvedValueOnce({ revision: 2, region_id: "box" })
      .mockResolvedValueOnce({ ...fixture(), revision: 3 });
    const region = {
      id: "box",
      page: 0,
      polygon: [
        { x: 0.1, y: 0.1 },
        { x: 0.2, y: 0.1 },
        { x: 0.2, y: 0.2 },
      ],
      source: "manual" as const,
      entity_id: null,
      selected: true,
      text: "",
      score: null,
      rotation: 0,
      replacement: "已脱敏",
    };
    useWorkbench.getState().replaceRegions([region]);
    useWorkbench
      .getState()
      .change(fixture().entities.map((e) => ({ ...e, selected: false })));
    const regionSave = saveRegion("task", region);
    await flushReview();
    await regionSave;
    expect(vi.mocked(call).mock.calls.map(([command]) => command)).toEqual([
      "upsert_region",
      "review_patch",
    ]);
    expect(vi.mocked(call).mock.calls[1][1]).toMatchObject({
      expectedRevision: 2,
    });
  });
  it("retries a lost response with the same mutation ID and blocks navigation on failure", async () => {
    useWorkbench
      .getState()
      .change(fixture().entities.map((e) => ({ ...e, selected: false })));
    vi.mocked(call).mockRejectedValueOnce(Error("连接中断"));
    await expect(flushReview()).rejects.toThrow("连接中断");
    expect(useWorkbench.getState().dirty).toBe(true);
    const sent = vi.mocked(call).mock.calls[0][1];
    vi.mocked(call).mockResolvedValueOnce({ ...fixture(), revision: 2 });
    await retryReview();
    expect(vi.mocked(call).mock.calls[1][1]).toEqual(sent);
    expect(useWorkbench.getState().saveError).toBe("");
  });
});
