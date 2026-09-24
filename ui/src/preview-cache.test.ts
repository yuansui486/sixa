import { beforeEach, describe, expect, it, vi } from "vitest";
import { call } from "./api";
import { acquirePage, clearPreviewCache } from "./preview-cache";
vi.mock("./api", () => ({ call: vi.fn() }));
beforeEach(() => {
  clearPreviewCache();
  vi.mocked(call).mockReset();
  let nextUrl = 0;
  vi.spyOn(URL, "createObjectURL").mockImplementation(
    () => `blob:test-${nextUrl++}`,
  );
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
});
describe("page preview cache", () => {
  it("evicts unreferenced decoded pages when their estimated size exceeds 96 MiB", async () => {
    vi.mocked(call).mockResolvedValue([1]);
    for (let page = 0; page < 18; page++) {
      const cached = await acquirePage("large", false, page, 1, 1400, 1400);
      cached.release();
    }
    expect(URL.revokeObjectURL).toHaveBeenCalled();
    const newest = await acquirePage("large", false, 17, 1, 1400, 1400);
    newest.release();
    expect(call).toHaveBeenCalledTimes(18);
    const evicted = await acquirePage("large", false, 0, 1, 1400, 1400);
    evicted.release();
    expect(call).toHaveBeenCalledTimes(19);
  });
  it("limits native rendering to two concurrent requests and reuses identical pages", async () => {
    const releases: ((value: number[]) => void)[] = [];
    vi.mocked(call).mockImplementation(
      () =>
        new Promise((resolve) => {
          releases.push(resolve as (value: number[]) => void);
        }),
    );
    const one = acquirePage("task", false, 0, 1, 600, 840);
    const duplicate = acquirePage("task", false, 0, 1, 600, 840);
    const two = acquirePage("task", false, 1, 1, 600, 840);
    const three = acquirePage("task", false, 2, 1, 600, 840);
    await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
    releases[0]([1, 2, 3]);
    await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(3));
    releases[1]([1]);
    releases[2]([1]);
    const entries = await Promise.all([one, duplicate, two, three]);
    expect(entries[0].url).toBe(entries[1].url);
    entries.forEach((entry) => entry.release());
    const reused = await acquirePage("task", false, 0, 1, 600, 840);
    reused.release();
    expect(call).toHaveBeenCalledTimes(3);
  });
  it("revokes cached URLs when a task is removed and reloads on next access", async () => {
    vi.mocked(call).mockResolvedValue([1, 2]);
    const first = await acquirePage("removed", false, 0, 1, 600, 840);
    first.release();
    clearPreviewCache("removed");
    expect(URL.revokeObjectURL).toHaveBeenCalled();
    const second = await acquirePage("removed", false, 0, 1, 600, 840);
    second.release();
    expect(call).toHaveBeenCalledTimes(2);
  });
  it("invalidates only the removed task while another task is rendering", async () => {
    const requests = deferredPages();
    const removed = acquirePage("removed", false, 0, 1, 600, 840);
    const other = acquirePage("other", false, 0, 1, 600, 840);
    const rejected = expect(removed).rejects.toThrow("预览已关闭");
    await vi.waitFor(() => expect(requests).toHaveLength(2));
    clearPreviewCache("removed");
    requests[0]([1]);
    requests[1]([2]);
    await rejected;
    const page = await other;
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    const reused = await acquirePage("other", false, 0, 1, 600, 840);
    expect(reused.url).toBe(page.url);
    expect(call).toHaveBeenCalledTimes(2);
    page.release();
    reused.release();
  });
  it("does not reuse an invalidated request or let its finalizer remove a new request", async () => {
    const requests = deferredPages();
    const old = acquirePage("task", false, 0, 1, 600, 840);
    const rejected = expect(old).rejects.toThrow("预览已关闭");
    await vi.waitFor(() => expect(requests).toHaveLength(1));
    clearPreviewCache("task");
    const fresh = acquirePage("task", false, 0, 1, 600, 840);
    await vi.waitFor(() => expect(requests).toHaveLength(2));
    requests[0]([1]);
    await rejected;
    const duplicate = acquirePage("task", false, 0, 1, 600, 840);
    await Promise.resolve();
    expect(call).toHaveBeenCalledTimes(2);
    requests[1]([2]);
    const pages = await Promise.all([fresh, duplicate]);
    expect(pages[0].url).toBe(pages[1].url);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    pages.forEach((page) => page.release());
  });
  it("cancels queued requests on logout without resetting occupied native slots", async () => {
    const requests = deferredPages();
    const activeOne = acquirePage("one", false, 0, 1, 600, 840);
    const activeTwo = acquirePage("two", false, 0, 1, 600, 840);
    const queuedOne = acquirePage("old-queued", false, 0, 1, 600, 840);
    const queuedTwo = acquirePage("old-queued", false, 1, 1, 600, 840);
    const settled = Promise.allSettled([
      activeOne,
      activeTwo,
      queuedOne,
      queuedTwo,
    ]);
    await vi.waitFor(() => expect(requests).toHaveLength(2));
    clearPreviewCache();
    await expect(queuedOne).rejects.toThrow("预览已关闭");
    await expect(queuedTwo).rejects.toThrow("预览已关闭");
    const fresh = acquirePage("fresh", false, 0, 1, 600, 840);
    await Promise.resolve();
    expect(call).toHaveBeenCalledTimes(2);
    requests[0]([1]);
    await vi.waitFor(() => expect(requests).toHaveLength(3));
    expect(
      vi.mocked(call).mock.calls.map(([, args]) => (args as { id: string }).id),
    ).toEqual(["one", "two", "fresh"]);
    requests[1]([2]);
    requests[2]([3]);
    expect(
      (await settled).every((result) => result.status === "rejected"),
    ).toBe(true);
    const page = await fresh;
    page.release();
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
  });
  it("checks validity before starting IPC even when the slot was immediately available", async () => {
    vi.mocked(call).mockResolvedValue([1]);
    const stale = acquirePage("task", false, 0, 1, 600, 840);
    clearPreviewCache();
    await expect(stale).rejects.toThrow("预览已关闭");
    expect(call).not.toHaveBeenCalled();
    const fresh = await acquirePage("task", false, 0, 1, 600, 840);
    fresh.release();
    expect(call).toHaveBeenCalledTimes(1);
  });
  it("does not return an already revoked URL if clearing follows native completion", async () => {
    const requests = deferredPages();
    const stale = acquirePage("task", false, 0, 1, 600, 840);
    const rejected = expect(stale).rejects.toThrow("预览已关闭");
    await vi.waitFor(() => expect(requests).toHaveLength(1));
    requests[0]([1]);
    queueMicrotask(() => clearPreviewCache("task"));
    await rejected;
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
  });
  it("keeps a shared page pinned when one consumer releases twice", async () => {
    vi.mocked(call).mockResolvedValue([1]);
    const one = await acquirePage("shared", false, 0, 1, 1400, 1400);
    const two = await acquirePage("shared", false, 0, 1, 1400, 1400);
    one.release();
    one.release();
    for (let page = 1; page <= 18; page++) {
      const extra = await acquirePage("shared", false, page, 1, 1400, 1400);
      extra.release();
    }
    expect(URL.revokeObjectURL).not.toHaveBeenCalledWith(two.url);
    const stillPinned = await acquirePage("shared", false, 0, 1, 1400, 1400);
    expect(stillPinned.url).toBe(two.url);
    expect(call).toHaveBeenCalledTimes(19);
    two.release();
    stillPinned.release();
  });
});

function deferredPages() {
  const requests: ((value: number[]) => void)[] = [];
  vi.mocked(call).mockImplementation(
    () =>
      new Promise((resolve) => {
        requests.push(resolve as (value: number[]) => void);
      }),
  );
  return requests;
}

it("separates draft pixels from original and generated output", async () => {
  vi.mocked(call).mockResolvedValue([1]);
  const original = await acquirePage("task", false, 0, 4, 600, 840);
  const draft = await acquirePage("task", false, 0, 4, 600, 840, 1400, {
    draft: true,
  });
  const output = await acquirePage("task", true, 0, 4, 600, 840);
  expect(new Set([original.url, draft.url, output.url]).size).toBe(3);
  expect(call).toHaveBeenCalledWith(
    "document_draft_page",
    expect.objectContaining({
      expectedRevision: 4,
      requestId: expect.any(String),
    }),
  );
  [original, draft, output].forEach((page) => page.release());
});

it("cancels native draft work only when its last consumer leaves", async () => {
  let finish!: (value: number[]) => void;
  vi.mocked(call).mockImplementation((command) =>
    command === "cancel_document_preview"
      ? Promise.resolve(undefined)
      : new Promise((resolve) => {
          finish = resolve as typeof finish;
        }),
  );
  const firstController = new AbortController();
  const secondController = new AbortController();
  const first = acquirePage("draft", false, 0, 1, 600, 840, 1400, {
    draft: true,
    signal: firstController.signal,
  });
  const second = acquirePage("draft", false, 0, 1, 600, 840, 1400, {
    draft: true,
    signal: secondController.signal,
  });
  const rejectedFirst = expect(first).rejects.toThrow("预览已关闭");
  const rejectedSecond = expect(second).rejects.toThrow("预览已关闭");
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(1));
  firstController.abort();
  await rejectedFirst;
  expect(call).toHaveBeenCalledTimes(1);
  secondController.abort();
  await rejectedSecond;
  expect(call).toHaveBeenCalledWith("cancel_document_preview", {
    requestId: expect.any(String),
  });
  finish([1]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(URL.createObjectURL).not.toHaveBeenCalled();
});

it("keeps a surviving consumer's shared page usable after another aborts", async () => {
  const requests = deferredPages();
  const controller = new AbortController();
  const leaving = acquirePage("shared", false, 0, 1, 600, 840, 1400, {
    signal: controller.signal,
  });
  const staying = acquirePage("shared", false, 0, 1, 600, 840);
  const rejected = expect(leaving).rejects.toThrow("预览已关闭");
  await vi.waitFor(() => expect(requests).toHaveLength(1));
  controller.abort();
  await rejected;
  requests[0]([1]);
  const page = await staying;
  expect(URL.revokeObjectURL).not.toHaveBeenCalledWith(page.url);
  page.release();
});

it("reuses immutable source pixels across review revisions", async () => {
  vi.mocked(call).mockResolvedValue([1]);
  const original = await acquirePage("immutable", false, 0, 1, 600, 840);
  const updated = await acquirePage("immutable", false, 0, 9, 600, 840);
  expect(original.url).toBe(updated.url);
  expect(call).toHaveBeenCalledTimes(1);
  original.release();
  updated.release();
});
