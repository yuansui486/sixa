// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { call, type AppUpdateStatus } from "./api";
import { flushReview } from "./review";
import { Updates, UpdateEntry, useUpdates } from "./Updates";
const events = vi.hoisted(
  () => new Set<(event: { payload: AppUpdateStatus }) => void>(),
);
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name, callback) => {
    events.add(callback);
    return () => events.delete(callback);
  }),
}));
vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  call: vi.fn(),
}));
vi.mock("./review", () => ({ flushReview: vi.fn() }));
const status = (
  phase: AppUpdateStatus["phase"] = "available",
  revision = 1,
): AppUpdateStatus => ({
  revision,
  current_version: "1.0.9",
  version: "1.0.10",
  notes: "改进预览",
  phase,
  automatic: true,
  last_check: 1,
  downloaded: 0,
  total: null,
  bytes_per_second: 0,
  eta_seconds: null,
  error: null,
});
beforeEach(() => {
  events.clear();
  vi.mocked(call).mockReset();
  vi.mocked(flushReview).mockReset();
  vi.mocked(flushReview).mockResolvedValue();
  useUpdates.setState({
    status: null,
    open: false,
    dismissed: null,
    action: "",
    error: "",
  });
  vi.mocked(call).mockResolvedValue(status());
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});
afterEach(cleanup);
async function mount(phase: AppUpdateStatus["phase"] = "available") {
  vi.mocked(call).mockResolvedValue(status(phase));
  render(
    <>
      <Updates />
      <UpdateEntry settings />
    </>,
  );
  await waitFor(() => expect(useUpdates.getState().status?.phase).toBe(phase));
  act(() => useUpdates.setState({ open: true }));
}
it("发现更新只提示，确认前不下载", async () => {
  await mount();
  expect(vi.mocked(call).mock.calls.map(([name]) => name)).not.toContain(
    "download_app_update",
  );
  fireEvent.click(screen.getByRole("button", { name: "下载更新" }));
  await waitFor(() => expect(call).toHaveBeenCalledWith("download_app_update"));
});
it("保存失败阻止安装，并保留中文错误", async () => {
  await mount("ready");
  vi.mocked(flushReview).mockRejectedValueOnce(Error("复核修改尚未保存"));
  fireEvent.click(screen.getByRole("button", { name: "重启并安装" }));
  await screen.findByRole("alert");
  expect(screen.getByRole("alert").textContent).toContain("尚未保存");
  expect(vi.mocked(call).mock.calls.map(([name]) => name)).not.toContain(
    "install_app_update",
  );
});
it("先完成保存再调用安装，任务繁忙时可以稍后重试", async () => {
  await mount("ready");
  let saved!: () => void;
  vi.mocked(flushReview).mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        saved = resolve;
      }),
  );
  vi.mocked(call).mockRejectedValueOnce(Error("仍有处理任务，请等待完成"));
  fireEvent.click(screen.getByRole("button", { name: "重启并安装" }));
  expect(vi.mocked(call).mock.calls.map(([name]) => name)).not.toContain(
    "install_app_update",
  );
  await act(async () => saved());
  await screen.findByRole("alert");
  expect(call).toHaveBeenCalledWith("install_app_update");
  expect(
    screen.getByRole("button", { name: "稍后" }).hasAttribute("disabled"),
  ).toBe(false);
});
it("未知大小不显示假百分比，下载允许取消，晚到事件不回退状态", async () => {
  await mount("downloading");
  expect(screen.getByRole("progressbar").hasAttribute("value")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "取消下载" }));
  await waitFor(() => expect(call).toHaveBeenCalledWith("cancel_app_update"));
  act(() => {
    events.forEach((callback) => callback({ payload: status("ready", 3) }));
    events.forEach((callback) =>
      callback({ payload: status("downloading", 2) }),
    );
  });
  expect(useUpdates.getState().status?.phase).toBe("ready");
});
