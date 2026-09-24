// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { acquirePage } from "./preview-cache";
import { draftImages, originalImages } from "./docx-preview-images";
import type { TaskView } from "./api";
vi.mock("./preview-cache", () => ({ acquirePage: vi.fn() }));
beforeEach(() => {
  vi.mocked(acquirePage).mockReset();
});
function setup() {
  const body = document.createElement("div");
  body.innerHTML =
    '<section class="sixa-word"><div style="position:relative"><img data-image-index="0" src="blob:source-zero" style="transform:rotate(20deg);clip-path:inset(2px)"></div><div><img data-image-index="1" src="blob:source-one"></div></section><section class="sixa-word"><div><img data-image-index="0" src="blob:source-repeat"></div><div><img data-image-index="2" src="blob:source-two"></div></section>';
  const task = {
    meta: { id: "task" },
    entities: [],
    regions: [0, 1, 2].map((page) => ({ page, selected: true })),
  } as unknown as TaskView;
  return {
    body,
    originals: originalImages(body),
    task,
    enabled: true,
    ready: true,
    revision: 3,
    page: 0,
    retry: vi.fn(),
  };
}
describe("DOCX embedded image draft effects", () => {
  it("hides pending originals, requests only current-page images serially, and preserves cropping", async () => {
    const requests: ((value: { url: string; release: () => void }) => void)[] =
      [];
    vi.mocked(acquirePage).mockImplementation(
      () => new Promise((resolve) => requests.push(resolve)),
    );
    const input = setup();
    const dispose = draftImages(input);
    const images = [...input.body.querySelectorAll("img")];
    expect(images.every((image) => image.style.visibility === "hidden")).toBe(
      true,
    );
    expect(acquirePage).toHaveBeenCalledTimes(1);
    expect(acquirePage).toHaveBeenCalledWith(
      "task",
      false,
      0,
      3,
      1400,
      1400,
      1400,
      expect.objectContaining({ draft: true, signal: expect.any(AbortSignal) }),
    );
    const release = vi.fn();
    requests[0]({ url: "blob:draft-zero", release });
    await vi.waitFor(() => expect(acquirePage).toHaveBeenCalledTimes(2));
    expect(images[0].getAttribute("src")).toBe("blob:draft-zero");
    expect(images[0].style.transform).toBe("rotate(20deg)");
    expect(images[0].style.clipPath).toBe("inset(2px)");
    expect(images[2].style.visibility).toBe("hidden");
    requests[1]({ url: "blob:draft-one", release });
    await vi.waitFor(() =>
      expect(images[1].getAttribute("src")).toBe("blob:draft-one"),
    );
    expect(acquirePage).toHaveBeenCalledTimes(2);
    dispose();
    expect(release).toHaveBeenCalledTimes(2);
    expect(images[0].getAttribute("src")).toBe("blob:source-zero");
    expect(input.body.querySelector(".sixa-docx-image-state")).toBeNull();
  });
  it("waits for saved/debounced state and never presents the original as a finished draft", () => {
    const input = setup();
    const dispose = draftImages({ ...input, ready: false });
    expect(acquirePage).not.toHaveBeenCalled();
    expect(input.body.textContent).toContain("等待保存并更新图片效果");
    expect(input.body.querySelector("img")!.style.visibility).toBe("hidden");
    dispose();
  });
  it("releases a stale result when the review changes while rendering", async () => {
    let finish!: (value: { url: string; release: () => void }) => void;
    vi.mocked(acquirePage).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const input = setup();
    const dispose = draftImages(input);
    const signal = vi.mocked(acquirePage).mock.calls[0][7]!.signal;
    dispose();
    expect(signal?.aborted).toBe(true);
    const release = vi.fn();
    finish({ url: "blob:stale", release });
    await vi.waitFor(() => expect(release).toHaveBeenCalledTimes(1));
    expect(input.body.querySelector("img")!.getAttribute("src")).toBe(
      "blob:source-zero",
    );
    expect(acquirePage).toHaveBeenCalledTimes(1);
  });
});
