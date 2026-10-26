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
import { open, save } from "@tauri-apps/plugin-dialog";
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
it("保存失败显示重试，不能打开不存在的文件", async () => {
  current.error = "处理已完成，保存失败：磁盘空间不足";
  current.exists = false;
  mount();
  await screen.findByText("保存失败");
  expect((screen.getByText("打开文件") as HTMLButtonElement).disabled).toBe(
    true,
  );
  fireEvent.click(screen.getByText("重试保存"));
  expect((await screen.findByRole("alert")).textContent).toContain(
    "磁盘空间不足",
  );
  expect(screen.queryByText("文件已保存")).toBeNull();
});
it("删除任务后仍可另存实际文件", async () => {
  current.task_exists = false;
  vi.mocked(save).mockResolvedValue("C:/另存.pdf");
  mount();
  await screen.findByText(item.name);
  expect((screen.getByText("预览") as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByText("另存为"));
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("copy_output_file", {
      id: "file",
      path: "C:/另存.pdf",
    }),
  );
});
it("被移动的文件明确标注且不能打开", async () => {
  current.exists = false;
  mount();
  await screen.findByText("已移动或删除");
  expect((screen.getByText("所在文件夹") as HTMLButtonElement).disabled).toBe(
    true,
  );
});
it("工作台不能把自动保存失败显示为成功", async () => {
  current.error = "保存失败";
  current.exists = false;
  mount(<FileSaveNotice taskId="task" />);
  expect((await screen.findByRole("alert")).textContent).toContain("重试保存");
  expect(screen.queryByText(/已自动保存/)).toBeNull();
});
