// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { call } from "./api";
import { SaveResult } from "./SaveResult";

vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  call: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true,
    value() {
      this.open = true;
    },
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true,
    value() {
      this.open = false;
    },
  });
  vi.mocked(call).mockImplementation(async (name) => {
    if (name === "get_last_output_location") return "team/客户A";
    if (name === "browse_directory")
      return {
        root: "C:/输出",
        path: "C:/输出/team/客户A",
        relative: "team/客户A",
        items: [],
        total: 0,
      };
    if (name === "save_result_to_last")
      return {
        id: "output",
        task_id: "task",
        name: "合同_脱敏.pdf",
        kind: "pdf",
        path: "C:/输出/team/客户A/合同_脱敏.pdf",
        created_at: 1,
        size: 20,
        error: null,
        generation: "1",
        partial: false,
        failed_count: 0,
        source: "manual",
        exists: true,
        task_exists: true,
      };
    return undefined;
  });
});

afterEach(cleanup);

it("保存到上次位置并把真实子目录返回给调用方", async () => {
  const onSaved = vi.fn();
  render(
    <QueryClientProvider client={new QueryClient()}>
      <SaveResult
        target={{ id: "task", name: "合同_脱敏.pdf" }}
        onClose={vi.fn()}
        onSaved={onSaved}
      />
    </QueryClientProvider>,
  );

  fireEvent.click(
    await screen.findByRole("button", { name: "保存到上次位置" }),
  );
  await waitFor(() =>
    expect(onSaved).toHaveBeenCalledWith({
      path: "C:/输出/team/客户A/合同_脱敏.pdf",
      relative: "team/客户A",
      external: false,
    }),
  );
  expect(
    vi
      .mocked(call)
      .mock.calls.find(([name]) => name === "save_result_to_last")?.[1],
  ).toMatchObject({
    id: "task",
    name: "合同_脱敏.pdf",
  });
});
