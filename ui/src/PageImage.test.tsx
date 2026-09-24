// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PageImage } from "./PageImage";
import { acquirePage } from "./preview-cache";
vi.mock("./preview-cache", () => ({ acquirePage: vi.fn() }));
const page = { index: 0, width: 600, height: 840, preview_uri: "" };
beforeEach(() => {
  vi.mocked(acquirePage).mockReset();
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(
        private callback: (entries: { isIntersecting: boolean }[]) => void,
      ) {}
      observe() {
        this.callback([{ isIntersecting: true }]);
      }
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
it("does not render a disabled draft and never substitutes the source URI", async () => {
  render(
    <PageImage
      id="task"
      result={false}
      revision={1}
      page={{ ...page, preview_uri: "data:image/png;base64,source" }}
      draft
      enabled={false}
    />,
  );
  expect(screen.queryByRole("img")).toBeNull();
  expect(acquirePage).not.toHaveBeenCalled();
});
it("does not let a superseded completion replace the current revision", async () => {
  const finish: ((value: { url: string; release: () => void }) => void)[] = [];
  vi.mocked(acquirePage).mockImplementation(
    () => new Promise((resolve) => finish.push(resolve)),
  );
  const view = render(
    <PageImage id="task" result={false} revision={1} page={page} draft />,
  );
  await waitFor(() => expect(finish).toHaveLength(1));
  view.rerender(
    <PageImage id="task" result={false} revision={2} page={page} draft />,
  );
  await waitFor(() => expect(finish).toHaveLength(2));
  const releaseOld = vi.fn();
  await act(async () => {
    finish[1]({ url: "blob:new", release: vi.fn() });
  });
  await act(async () => {
    finish[0]({ url: "blob:old", release: releaseOld });
  });
  expect(screen.getByRole("img").getAttribute("src")).toBe("blob:new");
  expect(releaseOld).toHaveBeenCalledOnce();
});
it("clears old pixels while a changed effect is being saved", async () => {
  const release = vi.fn();
  vi.mocked(acquirePage).mockResolvedValue({ url: "blob:old", release });
  const view = render(
    <PageImage id="task" result={false} revision={1} page={page} draft />,
  );
  await screen.findByRole("img");
  view.rerender(
    <PageImage
      id="task"
      result={false}
      revision={1}
      page={page}
      draft
      enabled={false}
    />,
  );
  expect(screen.queryByRole("img")).toBeNull();
  expect(release).toHaveBeenCalledOnce();
});
it("shows an output error with retry instead of source pixels", async () => {
  vi.mocked(acquirePage).mockRejectedValue(new Error("结果文件损坏"));
  render(<PageImage id="task" result revision={2} page={page} />);
  await screen.findByText(/结果文件损坏/);
  expect(screen.queryByRole("img")).toBeNull();
  expect(screen.getByRole("button", { name: "重试此页" })).toBeTruthy();
});

