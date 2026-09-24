import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { LoaderCircle } from "lucide-react";
import { call, message, type CloseAction, type CloseRequest } from "./api";
import { flushReview, retryReview } from "./review";

// This component stays mounted across login/logout. Hiding the native window
// therefore leaves both the document and the review queue intact.
export function Lifecycle() {
  const client = useQueryClient();
  const [request, setRequest] = useState<CloseRequest | null>(null);
  const [remember, setRemember] = useState(false);
  const [error, setError] = useState("");
  const [saveError, setSaveError] = useState("");
  const [actionPending, setActionPending] = useState(false);
  const [saving, setSaving] = useState(false);
  const [savingSlow, setSavingSlow] = useState(false);
  const [waitCount, setWaitCount] = useState(0);
  const dialog = useRef<HTMLDialogElement>(null);
  const previousFocus = useRef<HTMLElement | null>(null);
  const latest = useRef<CloseRequest | null>(null);
  const eventVersion = useRef(0);
  const mounted = useRef(false);
  const actionSerial = useRef(0);
  const started = useRef(new Set<string>());
  const saveInFlight = useRef<Promise<void> | null>(null);

  const accept = (next: CloseRequest | null) => {
    const previous = latest.current;
    latest.current = next;
    setRequest(next);
    if (previous?.id !== next?.id) {
      started.current.clear();
      actionSerial.current++;
      setActionPending(false);
      setRemember(false);
      setSaving(false);
      setSavingSlow(false);
      setWaitCount(0);
    }
    if (previous?.id !== next?.id || previous?.phase !== next?.phase) {
      setError("");
      setSaveError("");
    }
  };
  const acknowledge = (next: CloseRequest | null) => {
    if (!next) return;
    void call("acknowledge_app_close", { requestId: next.id }).catch(
      (error) => {
        if (mounted.current && latest.current?.id === next.id)
          setError(message(error));
      },
    );
  };

  const respond = async (
    action: CloseAction,
    target = latest.current,
    persist = false,
  ) => {
    if (!target || latest.current?.id !== target.id) return;
    const serial = ++actionSerial.current;
    const receivedEvents = eventVersion.current;
    setActionPending(true);
    setError("");
    try {
      const next = await call("respond_app_close", {
        requestId: target.id,
        action,
        ...(action === "tray" || action === "exit"
          ? { remember: persist }
          : {}),
      });
      if (
        !mounted.current ||
        latest.current?.id !== target.id ||
        serial !== actionSerial.current
      )
        return;
      // Native events can advance the request while the command is returning.
      // A terminal reply is authoritative; an older phase must not replace an event.
      if (!next || receivedEvents === eventVersion.current) accept(next);
      if (persist)
        void client.invalidateQueries({ queryKey: ["desktop-preferences"] });
    } catch (error) {
      if (
        mounted.current &&
        latest.current?.id === target.id &&
        serial === actionSerial.current
      )
        setError(message(error));
    } finally {
      if (mounted.current && serial === actionSerial.current)
        setActionPending(false);
    }
  };

  const saveBeforeExit = async (target: CloseRequest, retry = false) => {
    if (latest.current?.id !== target.id || latest.current.phase !== "saving")
      return;
    setSaving(true);
    setSaveError("");
    if (!saveInFlight.current) {
      const operation = retry ? retryReview() : flushReview();
      saveInFlight.current = operation;
      void operation
        .finally(() => {
          if (saveInFlight.current === operation) saveInFlight.current = null;
        })
        .catch(() => undefined);
    }
    try {
      await saveInFlight.current;
      if (
        mounted.current &&
        latest.current?.id === target.id &&
        latest.current.phase === "saving"
      )
        await respond("saved", target);
    } catch (error) {
      if (
        mounted.current &&
        latest.current?.id === target.id &&
        latest.current.phase === "saving"
      )
        setSaveError(message(error));
    } finally {
      if (mounted.current && latest.current?.id === target.id) setSaving(false);
    }
  };

  useEffect(() => {
    mounted.current = true;
    let active = true;
    const closeSubscription = listen<CloseRequest | null>(
      "app-close-request",
      ({ payload }) => {
        if (!active) return;
        eventVersion.current++;
        accept(payload);
        acknowledge(payload);
      },
    );
    const settingsSubscription = listen("app-open-settings", () => {
      if (!active) return;
      void flushReview()
        .then(() => {
          // HashRouter is mounted only after login. The hash also preserves this
          // destination while the login form is visible.
          if (active) window.location.hash = "/settings";
        })
        .catch((error) => {
          if (active) setError(message(error));
        });
    });
    void closeSubscription
      .then(async () => {
        if (!active) return;
        const before = eventVersion.current;
        const pending = await call("get_close_request");
        if (active && before === eventVersion.current) {
          accept(pending);
          acknowledge(pending);
        }
      })
      .catch((error) => {
        if (active) setError(message(error));
      });
    return () => {
      active = false;
      mounted.current = false;
      void closeSubscription
        .then((dispose) => dispose())
        .catch(() => undefined);
      void settingsSubscription
        .then((dispose) => dispose())
        .catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    if (request?.phase !== "saving" || started.current.has(request.id)) return;
    started.current.add(request.id);
    void saveBeforeExit(request);
  }, [request?.id, request?.phase]);
  useEffect(() => {
    setSavingSlow(false);
    if (request?.phase !== "saving") return;
    const timer = window.setTimeout(() => setSavingSlow(true), 5000);
    return () => window.clearTimeout(timer);
  }, [request?.id, request?.phase, waitCount]);
  useEffect(() => {
    const modal = dialog.current;
    if (request && modal && !modal.open) {
      previousFocus.current =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      modal.showModal();
    }
    if (!request) {
      if (modal?.open) modal.close();
      if (previousFocus.current?.isConnected) previousFocus.current.focus();
      previousFocus.current = null;
    }
  }, [request]);

  if (!request)
    return error ? (
      <div className="lifecycle-notice" role="alert">
        <span>{error}</span>
        <button className="text-action" onClick={() => setError("")}>
          关闭提示
        </button>
      </div>
    ) : null;
  const slow =
    request.phase === "slow" || (request.phase === "saving" && savingSlow);
  const heading =
    request.phase === "choice"
      ? "关闭私匣"
      : request.phase === "saving"
        ? saveError
          ? "修改还未保存"
          : "正在保存修改"
        : request.phase === "confirm"
          ? "仍有任务正在运行"
          : slow
            ? "停止任务需要一些时间"
            : "正在停止任务";
  return (
    <dialog
      className="close-dialog"
      ref={dialog}
      aria-labelledby="close-heading"
      onCancel={(event) => {
        event.preventDefault();
        void respond("cancel");
      }}
    >
      <h2 id="close-heading">{heading}</h2>
      {request.phase === "choice" ? (
        <>
          <p>
            {request.tray_available
              ? "可以保留私匣在后台运行，或直接退出应用。"
              : "当前系统无法使用托盘，可以直接退出应用。"}
          </p>
          {(request.active_tasks > 0 || request.active_downloads > 0) && (
            <p className="muted">
              当前有 {request.active_tasks} 个处理任务、
              {request.active_downloads} 个模型下载。
            </p>
          )}
          <label className="close-remember">
            <input
              type="checkbox"
              checked={remember}
              onChange={(event) => setRemember(event.target.checked)}
              disabled={actionPending}
            />
            不再提醒，可在应用设置中修改
          </label>
        </>
      ) : request.phase === "saving" ? (
        <>
          {saveError ? (
            <p role="alert" className="close-error">
              {saveError}。请重试保存，或取消退出以继续检查。
            </p>
          ) : (
            <p className="close-progress">
              <LoaderCircle className="spin" size={18} />
              正在保存当前复核修改，请稍候。
            </p>
          )}
        </>
      ) : request.phase === "confirm" ? (
        <p>
          当前有 {request.active_tasks} 个处理任务、{request.active_downloads}{" "}
          个模型下载。退出将停止这些操作，已生成的文件会保留。
        </p>
      ) : (
        <p className="close-progress">
          <LoaderCircle className="spin" size={18} />
          正在安全停止任务和下载。
        </p>
      )}
      {slow && (
        <p className="close-force-note">
          强制退出可能丢失尚未保存的修改，并中断正在写入的文件。
        </p>
      )}
      {error && (
        <p role="alert" className="close-error">
          {error}
        </p>
      )}
      <div className="close-actions">
        <button className="secondary" onClick={() => void respond("cancel")}>
          取消退出
        </button>
        {request.phase === "choice" && (
          <>
            <button
              className="secondary"
              disabled={actionPending}
              onClick={() => void respond("exit", request, remember)}
            >
              直接退出
            </button>
            <button
              disabled={actionPending || !request.tray_available}
              title={
                !request.tray_available ? "当前系统无法使用托盘" : undefined
              }
              onClick={() => void respond("tray", request, remember)}
            >
              最小化到托盘
            </button>
          </>
        )}
        {request.phase === "saving" && saveError && (
          <button
            disabled={saving || actionPending}
            onClick={() => void saveBeforeExit(request, true)}
          >
            重试保存
          </button>
        )}
        {request.phase === "confirm" && (
          <button disabled={actionPending} onClick={() => void respond("stop")}>
            停止并退出
          </button>
        )}
        {slow && (
          <>
            <button
              className="secondary"
              disabled={actionPending}
              onClick={() => {
                setWaitCount((count) => count + 1);
                void respond("wait");
              }}
            >
              继续等待
            </button>
            <button className="danger" onClick={() => void respond("force")}>
              强制退出
            </button>
          </>
        )}
      </div>
    </dialog>
  );
}
