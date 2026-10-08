import { useEffect, useRef } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { LoaderCircle, RefreshCw, X } from "lucide-react";
import { call, formatBytes, message, type AppUpdateStatus } from "./api";
import { flushReview } from "./review";

type UpdateState = {
  status: AppUpdateStatus | null;
  open: boolean;
  dismissed: string | null;
  action: string;
  error: string;
};
export const useUpdates = create<UpdateState>(() => ({
  status: null,
  open: false,
  dismissed: null,
  action: "",
  error: "",
}));
function accept(status: AppUpdateStatus) {
  if (!status) return;
  useUpdates.setState((old) =>
    old.status && old.status.revision > status.revision ? {} : { status },
  );
}
async function run(action: "check" | "download" | "cancel" | "install") {
  if (useUpdates.getState().action && action !== "cancel") return;
  useUpdates.setState({ action, error: "" });
  try {
    if (action === "install") {
      // The modal prevents new review edits while the queue is being drained.
      await flushReview();
      accept(await call("install_app_update"));
    } else if (action === "cancel") {
      await call("cancel_app_update");
    } else if (action === "download") {
      accept(await call("download_app_update"));
    } else {
      accept(await call("check_app_update"));
    }
  } catch (error) {
    useUpdates.setState({ error: message(error) });
  } finally {
    if (useUpdates.getState().action === action)
      useUpdates.setState({ action: "" });
  }
}

export function UpdateEntry({ settings = false }: { settings?: boolean }) {
  const status = useUpdates((s) => s.status);
  return (
    <section className={settings ? "card settings-section" : "update-entry"}>
      {settings && <h2>版本与更新</h2>}
      <div className="update-entry-line">
        <span>私匣 {status?.current_version ?? "1.0.9"}</span>
        <button
          type="button"
          className="secondary"
          onClick={() => {
            useUpdates.setState({ open: true, error: "" });
            if (!status || ["idle", "current", "failed"].includes(status.phase))
              void run("check");
          }}
        >
          {" "}
          {status?.phase === "ready" ? "安装更新" : "检查更新"}
        </button>
      </div>
      {settings && status && (
        <label className="update-auto">
          <input
            type="checkbox"
            checked={status.automatic}
            onChange={(event) => {
              void call("set_app_update_preferences", {
                automatic: event.target.checked,
              })
                .then(accept)
                .catch((error) =>
                  useUpdates.setState({ error: message(error), open: true }),
                );
            }}
          />
          自动检查更新（每天一次，确认后下载）
        </label>
      )}
    </section>
  );
}

const labels: Record<AppUpdateStatus["phase"], string> = {
  idle: "可检查是否有新版本",
  checking: "正在检查更新…",
  current: "当前已是最新版本",
  available: "发现新版本",
  downloading: "正在下载更新",
  verifying: "正在校验更新包…",
  ready: "更新已下载，重启后完成安装",
  installing: "正在安装，请稍候…",
  failed: "更新未完成",
};
export function Updates() {
  const { status, open, dismissed, action, error } = useUpdates();
  const dialog = useRef<HTMLDialogElement>(null);
  const busyInstall = action === "install" || status?.phase === "installing";
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<AppUpdateStatus>("app-update-progress", ({ payload }) => {
      if (!disposed) accept(payload);
    })
      .then((stop) => {
        if (disposed) {
          stop();
          return;
        }
        unlisten = stop;
        void call("get_app_update_status")
          .then((value) => {
            if (!disposed) accept(value);
          })
          .catch(() => undefined);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  useEffect(() => {
    if (open && !dialog.current?.open) dialog.current?.showModal();
    if (!open && dialog.current?.open) dialog.current?.close();
  }, [open]);
  const close = () => {
    if (!busyInstall)
      useUpdates.setState({ open: false, dismissed: status?.version ?? null });
  };
  const downloading =
    status?.phase === "downloading" || status?.phase === "verifying";
  const notice =
    status?.version &&
    dismissed !== status.version &&
    ["available", "ready"].includes(status.phase);
  return (
    <>
      {notice && !open && (
        <aside className="update-notice" aria-label="应用更新提示">
          <span>
            {status.phase === "ready"
              ? "更新已下载"
              : `私匣 ${status.version} 已发布`}
          </span>
          <button
            type="button"
            className="secondary"
            onClick={() => useUpdates.setState({ open: true })}
          >
            查看更新
          </button>
          <button
            type="button"
            className="icon-button secondary"
            aria-label="稍后提醒更新"
            onClick={close}
          >
            <X size={16} />
          </button>
        </aside>
      )}
      <dialog
        ref={dialog}
        className="close-dialog update-dialog"
        aria-labelledby="update-title"
        onCancel={(event) => {
          event.preventDefault();
          close();
        }}
      >
        <h2 id="update-title">应用更新</h2>
        <p className="muted">
          当前版本 {status?.current_version ?? "1.0.9"}
          {status?.version ? ` → ${status.version}` : ""}
        </p>
        <p className="update-phase" role="status">
          {(action === "install" ||
            downloading ||
            status?.phase === "checking") && (
            <LoaderCircle size={18} className="spin" />
          )}
          {action === "install" && status?.phase !== "installing"
            ? "正在保存复核修改…"
            : status
              ? labels[status.phase]
              : "正在读取更新状态…"}
        </p>
        {status?.notes && <div className="update-notes">{status.notes}</div>}
        {downloading && status && (
          <div className="update-download">
            <progress
              aria-label="更新下载进度"
              max={status.total || undefined}
              value={
                status.total
                  ? Math.min(status.downloaded, status.total)
                  : undefined
              }
            />
            <div>
              {formatBytes(status.downloaded)}
              {status.total
                ? ` / ${formatBytes(status.total)} · ${Math.floor(Math.min(status.downloaded / status.total, 1) * 100)}%`
                : ""}
              {status.bytes_per_second > 0
                ? ` · ${formatBytes(status.bytes_per_second)}/s`
                : ""}
              {status.eta_seconds != null && status.eta_seconds > 0
                ? ` · 约剩 ${status.eta_seconds < 60 ? `${status.eta_seconds} 秒` : `${Math.ceil(status.eta_seconds / 60)} 分钟`}`
                : ""}
            </div>
          </div>
        )}
        {(error || status?.error) && (
          <p className="error" role="alert">
            {error || status?.error}
          </p>
        )}
        {status?.phase === "ready" && (
          <p className="muted">
            重启前会保存复核修改。有任务运行时，请等待完成后再安装。
          </p>
        )}
        <div className="close-actions">
          {status?.version && ["ready", "failed"].includes(status.phase) && (
            <button
              type="button"
              className="secondary"
              disabled={!!action}
              onClick={() => void run("check")}
            >
              重新检查
            </button>
          )}
          <button
            type="button"
            className="secondary"
            disabled={busyInstall}
            onClick={close}
          >
            {downloading ? "后台下载" : "稍后"}
          </button>
          {downloading ? (
            <button
              type="button"
              className="secondary"
              onClick={() => void run("cancel")}
            >
              取消下载
            </button>
          ) : status?.phase === "ready" ? (
            <button
              type="button"
              disabled={!!action}
              onClick={() => void run("install")}
            >
              重启并安装
            </button>
          ) : status?.version && status.phase !== "checking" ? (
            <button
              type="button"
              disabled={!!action}
              onClick={() => void run("download")}
            >
              {status.phase === "failed" ? "重新下载" : "下载更新"}
            </button>
          ) : (
            <button
              type="button"
              disabled={!!action || status?.phase === "checking"}
              onClick={() => void run("check")}
            >
              <RefreshCw size={16} />
              检查更新
            </button>
          )}
        </div>
      </dialog>
    </>
  );
}
