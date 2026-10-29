// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { open } from "@tauri-apps/plugin-dialog";
import { call, type OutputFile } from "./api";
import { Files, FileSaveNotice } from "./Files";
vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  call: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("./review", () => ({ flushReview: vi.fn(async () => {}) }));
const item: OutputFile = {
  id: "file",
  task_id: "task",
  name: "合同_脱敏.pdf",
  kind: "pdf",
  path: "C:/结果/合同_脱敏.pdf",
  created_at: 1,
  size: 20,
  error: null,
  generation: "1",
  partial: false,
  failed_count: 0,
  source: "automatic",
  exists: true,
  task_exists: true,
};
let current: OutputFile;
beforeEach(() => {
  vi.clearAllMocks();
  current = { ...item };
  vi.mocked(call).mockImplementation(async (name) => {
    if (name === "get_file_directory") return "C:/结果";
    if (name === "get_last_output_location") return null;
    if (name === "browse_directory")
      return {
        root: "C:/结果",
        path: "C:/结果",
        relative: "",
        total: current.exists ? 1 : 0,
        items: current.exists
          ? [
              {
                name: current.name,
                path: current.path,
                directory: false,
                size: 20,
                modified: 1,
                output: current,
              },
            ]
          : [],
      };
    if (name === "query_output_files") return { items: [current], total: 1 };
    if (name === "retry_output_file") return current;
    return undefined;
  });
});
afterEach(cleanup);
function mount(component = <Files />) {
  render(
    <MemoryRouter>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        {component}
      </QueryClientProvider>
    </MemoryRouter>,
  );
}
it("取消选择目录不会改变设置", async () => {
  vi.mocked(open).mockResolvedValue(null);
  mount();
  await screen.findByText(item.name);
  fireEvent.click(screen.getByText("更改位置"));
  await waitFor(() => expect(open).toHaveBeenCalled());
  expect(
    vi.mocked(call).mock.calls.some(([name]) => name === "set_file_directory"),
  ).toBe(false);
});
it("被移动文件不在当前真实目录出现", async () => {
  current.exists = false;
  mount();
  await screen.findByText("当前文件夹没有符合条件的文件。");
  expect(screen.queryByText(item.name)).toBeNull();
});
it("删除任务后仍能打开真实文件", async () => {
  current.task_exists = false;
  mount();
  await screen.findByText(item.name);
  fireEvent.click(screen.getByText("打开文件"));
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("open_directory_entry", {
      relative: item.name,
    }),
  );
  expect(screen.queryByText("查看任务")).toBeNull();
});
it("工作台不能把自动保存失败显示为成功", async () => {
  current.error = "保存失败";
  current.exists = false;
  mount(<FileSaveNotice taskId="task" />);
  expect((await screen.findByRole("alert")).textContent).toContain("重试保存");
  expect(screen.queryByText(/已自动保存/)).toBeNull();
});
