// @vitest-environment jsdom
import React from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { call, type CloseRequest } from "./api";
import { flushReview, retryReview } from "./review";
import { Lifecycle } from "./Lifecycle";

const events = vi.hoisted(
  () => new Map<string, Set<(event: { payload: unknown }) => void>>(),
);
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (name: string, listener: (event: { payload: unknown }) => void) => {
      const listeners = events.get(name) ?? new Set();
      listeners.add(listener);
      events.set(name, listeners);
      return () => listeners.delete(listener);
    },
  ),
}));
vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  call: vi.fn(),
}));
vi.mock("./review", () => ({ flushReview: vi.fn(), retryReview: vi.fn() }));
const request = (
  id = "close-1",
  phase: CloseRequest["phase"] = "choice",
): CloseRequest => ({
  id,
  phase,
  tray_available: true,
  active_tasks: 0,
  active_downloads: 0,
});
const emit = (value: CloseRequest) =>
  act(() => {
    events
      .get("app-close-request")
      ?.forEach((listener) => listener({ payload: value }));
  });
function mount() {
  return render(
    <React.StrictMode>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <Lifecycle />
      </QueryClientProvider>
    </React.StrictMode>,
  );
}

beforeEach(() => {
  events.clear();
  vi.mocked(call).mockReset();
  vi.mocked(flushReview).mockReset();
  vi.mocked(retryReview).mockReset();
  vi.mocked(call).mockResolvedValue(null);
  vi.mocked(flushReview).mockResolvedValue();
  vi.mocked(retryReview).mockResolvedValue();
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
  location.hash = "";
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("desktop lifecycle", () => {
  it("acknowledges native requests and honors native cancellation events", async () => {
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit(request("native"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("acknowledge_app_close", {
        requestId: "native",
      }),
    );
    act(() => {
      events
        .get("app-close-request")
        ?.forEach((listener) => listener({ payload: null }));
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });
  it("restores a pending request before login with remember disabled and no native close calls", async () => {
    vi.mocked(call).mockImplementation(async (command) =>
      command === "get_close_request" ? request() : null,
    );
    mount();
    await screen.findByRole("dialog");
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(
      false,
    );
    fireEvent.click(screen.getByRole("button", { name: "最小化到托盘" }));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("respond_app_close", {
        requestId: "close-1",
        action: "tray",
        remember: false,
      }),
    );
    expect(flushReview).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
  it("only sends a remembered preference after the user explicitly checks it", async () => {
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit({ ...request("remember"), tray_available: false });
    expect(
      (screen.getByRole("button", { name: "最小化到托盘" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "直接退出" }));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("respond_app_close", {
        requestId: "remember",
        action: "exit",
        remember: true,
      }),
    );
  });

  it("saves only once for duplicate events, and ignores completion after cancellation", async () => {
    let finish!: () => void;
    vi.mocked(flushReview).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit(request("pending", "saving"));
    emit(request("pending", "saving"));
    await waitFor(() => expect(flushReview).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "取消退出" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await act(async () => finish());
    expect(
      vi
        .mocked(call)
        .mock.calls.filter(
          ([command, args]) =>
            command === "respond_app_close" &&
            (args as { action: string }).action === "saved",
        ),
    ).toHaveLength(0);
  });

  it("shares an existing save with a new request and only acknowledges the current ID", async () => {
    let finish!: () => void;
    vi.mocked(flushReview).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit(request("older", "saving"));
    await waitFor(() => expect(flushReview).toHaveBeenCalledTimes(1));
    emit(request("newer", "saving"));
    await act(async () => finish());
    expect(flushReview).toHaveBeenCalledTimes(1);
    expect(
      vi
        .mocked(call)
        .mock.calls.filter(([command]) => command === "respond_app_close"),
    ).toEqual([["respond_app_close", { requestId: "newer", action: "saved" }]]);
  });

  it("offers retry after a failed save, then acknowledges the repaired save", async () => {
    vi.mocked(flushReview).mockRejectedValueOnce(Error("复核保存失败"));
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit(request("retry", "saving"));
    await screen.findByText(/复核保存失败/);
    fireEvent.click(screen.getByRole("button", { name: "重试保存" }));
    await waitFor(() => expect(retryReview).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("respond_app_close", {
        requestId: "retry",
        action: "saved",
      }),
    );
  });

  it("after five seconds offers waiting or forcing without starting another save", async () => {
    vi.useFakeTimers();
    vi.mocked(flushReview).mockImplementation(() => new Promise(() => {}));
    vi.mocked(call).mockImplementation(async (command, args) =>
      command === "respond_app_close" &&
      (args as { action: string }).action === "wait"
        ? request("slow-save", "saving")
        : null,
    );
    mount();
    await act(async () => {});
    emit(request("slow-save", "saving"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(screen.getByRole("button", { name: "强制退出" })).toBeTruthy();
    expect(screen.getByText(/可能丢失尚未保存的修改/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "继续等待" }));
    await act(async () => {});
    expect(flushReview).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith("respond_app_close", {
      requestId: "slow-save",
      action: "wait",
    });
  });

  it("never replaces a newer native request with an older command response", async () => {
    let finish!: (value: CloseRequest) => void;
    vi.mocked(call).mockImplementation(async (command) =>
      command === "respond_app_close"
        ? new Promise((resolve) => {
            finish = resolve as typeof finish;
          })
        : null,
    );
    mount();
    await waitFor(() => expect(call).toHaveBeenCalledWith("get_close_request"));
    emit(request("older"));
    fireEvent.click(screen.getByRole("button", { name: "直接退出" }));
    emit(request("current"));
    await act(async () => finish(request("older", "saving")));
    expect(flushReview).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取消退出" }));
    expect(call).toHaveBeenLastCalledWith("respond_app_close", {
      requestId: "current",
      action: "cancel",
    });
  });
});
