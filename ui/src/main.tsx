import React, {
  lazy,
  Suspense,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createRoot } from "react-dom/client";
import {
  HashRouter,
  NavLink,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from "react-router-dom";
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { open, save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import {
  ShieldCheck,
  FileText,
  History,
  ScanSearch,
  SlidersHorizontal,
  Database,
  Undo2,
  FolderOpen,
  Download,
  KeyRound,
  Square,
  Eye,
  EyeOff,
  ChevronLeft,
  ChevronRight,
  Maximize2,
  Minimize2,
  Settings2,
  CircleAlert,
  LoaderCircle,
  PanelLeftClose,
  PanelLeftOpen,
  ZoomIn,
  ZoomOut,
  ScanLine,
  Columns2,
  Trash2,
  RotateCcw,
  ChevronsRight,
  ChevronsLeft,
  LogOut,
  UserRound,
  Cable,
  Copy,
  RefreshCw,
  CheckCircle2,
  XCircle,
} from "lucide-react";
import {
  call,
  message,
  stateLabels,
  type Entity,
  type DocumentPreview,
  type Region,
  type Rule,
  type TaskOptions,
  capabilityInstalled,
  formatBytes,
  type ModelStatus,
  type TaskMeta,
  type AuthStatus,
} from "./api";
import { useWorkbench } from "./store";
import "./style.css";
import { Batch } from "./Batch";
import { Settings } from "./Settings";
import { Lifecycle } from "./Lifecycle";
import { Tasks } from "./History";
import { useJobs } from "./jobs";
import sixaMark from "./assets/sixa-mark.svg";
import {
  flushReview,
  saveEntities,
  saveRegion,
  retryReview,
  undoReview,
  resetReview,
} from "./review";
import { PageImage } from "./PageImage";
import { VirtualList } from "./VirtualList";
import { clearPreviewCache } from "./preview-cache";
import { exportName, rememberDirectory, usePreferences } from "./preferences";
import type { DocxScrollAnchor } from "./DocxPreview";

const DocxPreview = lazy(() =>
  import("./DocxPreview").then((module) => ({ default: module.DocxPreview })),
);

const client = new QueryClient({
  defaultOptions: {
    queries: { retry: false, refetchOnWindowFocus: false, gcTime: 0 },
  },
});
type ModelProgressPayload = Partial<ModelStatus> & {
  id?: string;
  stage?: string;
  current?: number;
  total?: number;
  percent?: number;
  bytes_per_second?: number;
  eta_seconds?: number | null;
  source?: string | null;
  source_label?: string | null;
  message?: string;
};

function formatRemaining(seconds?: number | null): string {
  if (!seconds || seconds <= 0) return "";
  if (seconds < 60) return `${Math.ceil(seconds)} 秒`;
  if (seconds < 3600) return `${Math.ceil(seconds / 60)} 分钟`;
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.ceil((seconds % 3600) / 60);
  return `${hours} 小时${minutes ? ` ${minutes} 分钟` : ""}`;
}

function modelProgressDetails(progress: ModelProgressPayload): string {
  const downloading = [
    "connecting",
    "switching_source",
    "downloading",
    "retrying",
  ].includes(progress.stage ?? "");
  const remaining = formatRemaining(progress.eta_seconds);
  return [
    progress.total
      ? `已下载 ${formatBytes(progress.current)} / ${formatBytes(progress.total)}`
      : "",
    downloading
      ? `下载速度 ${progress.bytes_per_second ? `${formatBytes(progress.bytes_per_second)}/秒` : "计算中"}`
      : "",
    remaining ? `预计剩余 ${remaining}` : "",
  ]
    .filter(Boolean)
    .join(" · ");
}

function canCancelModelProgress(stage?: string): boolean {
  return ["connecting", "switching_source", "downloading", "retrying"].includes(
    stage ?? "",
  );
}

function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  async function run(fn: () => Promise<void>) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await fn();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, notice, setNotice, setError, run };
}
function Feedback({ error, notice }: { error: string; notice: string }) {
  return (
    <>
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {notice && (
        <p role="status" className="notice">
          {notice}
        </p>
      )}
    </>
  );
}
function AuthGate() {
  const [checking, setChecking] = useState(true);
  const [auth, setAuth] = useState<AuthStatus | null>(null);
  const [reason, setReason] = useState("");

  useEffect(() => {
    let active = true;
    call("auth_status")
      .then((status) => {
        if (!active) return;
        setAuth(status);
        setReason(status.reason ?? "");
      })
      .catch((error) => active && setReason(message(error)))
      .finally(() => active && setChecking(false));
    const subscription = listen<AuthStatus>(
      "auth-state-changed",
      ({ payload }) => {
        if (!active) return;
        setAuth(payload);
        setReason(payload.reason ?? "");
        if (!payload.authenticated) {
          resetReview();
          useJobs.setState({ jobs: {} });
          clearPreviewCache();
          documentPreviewCache.clear();
          client.clear();
          useWorkbench.getState().setTask(null);
          useWorkbench.getState().setBatchId(null);
        }
      },
    );
    return () => {
      active = false;
      void subscription.then((dispose) => dispose());
    };
  }, []);

  if (checking) {
    return (
      <div className="auth-loading" role="status" aria-live="polite">
        <LoaderCircle size={30} className="spin" />
        <strong>正在校验登录状态</strong>
      </div>
    );
  }
  if (!auth?.authenticated || !auth.subject) {
    return (
      <Login
        reason={reason}
        onAuthenticated={(status) => {
          setAuth(status);
          setReason("");
        }}
      />
    );
  }
  return (
    <HashRouter>
      <App
        auth={auth}
        onLogout={async () => {
          await flushReview();
          await call("auth_logout");
          resetReview();
          useJobs.setState({ jobs: {} });
          clearPreviewCache();
          documentPreviewCache.clear();
          client.clear();
          useWorkbench.getState().setTask(null);
          useWorkbench.getState().setBatchId(null);
          setAuth(null);
        }}
      />
    </HashRouter>
  );
}

function Login({
  reason,
  onAuthenticated,
}: {
  reason: string;
  onAuthenticated: (status: AuthStatus) => void;
}) {
  const [tenantCode, setTenantCode] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(reason);

  useEffect(() => setError(reason), [reason]);
  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!tenantCode.trim() || !username.trim() || !password || busy) return;
    setBusy(true);
    setError("");
    try {
      const status = await call("auth_login", {
        tenantCode: tenantCode.trim(),
        username: username.trim(),
        password,
      });
      setPassword("");
      onAuthenticated(status);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <main className="login-page">
      <section className="login-panel" aria-labelledby="login-title">
        <div className="login-brand">
          <img src={sixaMark} alt="" className="brand-mark" />
          <div>
            <h1 id="login-title">私匣</h1>
            <p>本机数据脱敏</p>
          </div>
        </div>
        <form onSubmit={submit}>
          <label>
            租户编码
            <input
              value={tenantCode}
              onChange={(event) => setTenantCode(event.target.value)}
              autoComplete="organization"
              autoFocus
              placeholder="请输入租户编码"
            />
          </label>
          <label>
            用户名
            <input
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              autoComplete="username"
              placeholder="请输入用户名"
            />
          </label>
          <label>
            密码
            <span className="password-field">
              <input
                type={showPassword ? "text" : "password"}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="current-password"
                placeholder="请输入密码"
              />
              <button
                type="button"
                className="icon-button secondary"
                aria-label={showPassword ? "隐藏密码" : "显示密码"}
                title={showPassword ? "隐藏密码" : "显示密码"}
                onClick={() => setShowPassword((value) => !value)}
              >
                {showPassword ? <EyeOff size={17} /> : <Eye size={17} />}
              </button>
            </span>
          </label>
          {error && (
            <p className="error" role="alert">
              {error}
            </p>
          )}
          <button
            className="login-submit"
            type="submit"
            disabled={
              !tenantCode.trim() || !username.trim() || !password || busy
            }
          >
            {busy && <LoaderCircle size={17} className="spin" />}
            {busy ? "正在登录" : "登录"}
          </button>
        </form>
      </section>
    </main>
  );
}

function App({
  auth,
  onLogout,
}: {
  auth: AuthStatus;
  onLogout: () => Promise<void>;
}) {
  const location = useLocation();
  const navigate = useNavigate();
  const preferences = usePreferences();
  const [navigationError, setNavigationError] = useState("");
  const activeTaskId = useWorkbench((state) => state.task?.meta.id ?? null);
  const inActiveWorkbench = location.pathname === "/" && !!activeTaskId;
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [preparing, setPreparing] = useState(true);
  const [prepareError, setPrepareError] = useState("");
  const [setupProgress, setSetupProgress] = useState<ModelProgressPayload>({
    stage: "checking",
    message: "正在检查已安装模型…",
  });
  const [activity, setActivity] = useState<{
    id?: string;
    stage: string;
    done?: number;
    total?: number;
    terminal?: boolean;
    failed?: boolean;
  } | null>(null);
  const activityTimer = useRef<number | null>(null);
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  useEffect(() => {
    if (preferences.focusMode && inActiveWorkbench) {
      setSidebarCollapsed(true);
    } else if (preferences.sidebar !== "auto") {
      setSidebarCollapsed(preferences.sidebar === "collapsed");
    } else if (inActiveWorkbench) {
      setSidebarCollapsed(true);
    } else if (!inActiveWorkbench) {
      setSidebarCollapsed(false);
    }
  }, [
    activeTaskId,
    inActiveWorkbench,
    preferences.sidebar,
    preferences.focusMode,
  ]);
  useEffect(() => {
    let active = true;
    const updateActivity = (
      payload: {
        id?: string;
        stage?: string;
        done?: number;
        total?: number;
        terminal?: boolean;
      },
      batch = false,
    ) => {
      if (payload.id)
        useJobs.getState().update({ ...payload, id: payload.id, batch });
      if (activityTimer.current !== null) {
        window.clearTimeout(activityTimer.current);
        activityTimer.current = null;
      }
      const terminal = payload.terminal === true;
      setActivity({
        ...payload,
        stage: batch
          ? batchStageLabel(payload.stage ?? "")
          : taskStageLabel(payload.stage ?? ""),
        terminal,
        failed: payload.stage === "failed",
      });
      if (terminal) {
        const id = payload.id;
        activityTimer.current = window.setTimeout(() => {
          setActivity((current) =>
            !current || current.id === id ? null : current,
          );
          activityTimer.current = null;
        }, 2500);
      }
    };
    const subscriptions = [
      listen<{
        id?: string;
        stage?: string;
        done?: number;
        total?: number;
        terminal?: boolean;
      }>("task-progress", ({ payload }) => updateActivity(payload)),
      listen<{
        id?: string;
        stage?: string;
        done?: number;
        total?: number;
        terminal?: boolean;
      }>("batch-progress", ({ payload }) => updateActivity(payload, true)),
      listen<TaskMeta>("task-updated", () => {
        client.invalidateQueries({ queryKey: ["tasks"] });
        client.invalidateQueries({ queryKey: ["batch"] });
      }),
      listen<ModelProgressPayload>("model-progress", ({ payload }) => {
        if (payload && typeof payload.ready === "boolean") {
          client.setQueryData(["model"], payload);
          client.invalidateQueries({ queryKey: ["model-packages"] });
        }
        if (
          payload.id !== "ppocrv4-accurate-v1" &&
          (payload.message || payload.stage)
        )
          setSetupProgress({
            id: payload.id,
            stage: payload.stage,
            current: payload.current,
            total: payload.total,
            percent: payload.percent,
            bytes_per_second: payload.bytes_per_second,
            eta_seconds: payload.eta_seconds,
            source: payload.source,
            source_label: payload.source_label,
            message: payload.message ?? modelStageLabel(payload.stage ?? ""),
          });
      }),
    ];
    void Promise.allSettled(subscriptions)
      .then(async () => {
        if (!active) return null;
        const current = await call("model_status");
        if (!active) return null;
        client.setQueryData(["model"], current);
        const installed = ["raner-v1", "ppocrv4-mobile-v1"].every((id) =>
          (current.capabilities ?? []).some(
            (capability) => capability.id === id && capability.installed,
          ),
        );
        if (installed) setPreparing(false);
        return call("ensure_default_models");
      })
      .then((m) => {
        if (!active || !m) return;
        client.setQueryData(["model"], m);
        setPrepareError("");
      })
      .catch((error) => {
        if (active) setPrepareError(message(error));
      })
      .finally(() => active && setPreparing(false));
    return () => {
      active = false;
      if (activityTimer.current !== null)
        window.clearTimeout(activityTimer.current);
      subscriptions.forEach(
        (subscription) => void subscription.then((dispose) => dispose()),
      );
    };
  }, []);
  return (
    <div
      className={`app ${sidebarCollapsed ? "sidebar-collapsed" : ""} ${inActiveWorkbench ? "workbench-active" : ""}`}
    >
      <aside>
        <div className="brand">
          <img src={sixaMark} alt="" className="brand-mark" />
          <div className="sidebar-label">
            私匣<small>Sixa · 本机数据脱敏</small>
          </div>
        </div>
        <nav
          onClickCapture={async (event) => {
            const anchor = (event.target as HTMLElement).closest("a");
            if (!anchor) return;
            event.preventDefault();
            try {
              await flushReview();
              setNavigationError("");
              navigate(anchor.hash.replace(/^#/, "") || "/");
            } catch (error) {
              setNavigationError(message(error));
            }
          }}
        >
          {[
            ["/", FileText, "脱敏工作台"],
            ["/batch", FolderOpen, "批量处理"],
            ["/history", History, "任务历史"],
            ["/rules", ScanSearch, "识别规则"],
            ["/policies", SlidersHorizontal, "脱敏方式"],
            ["/models", Database, "模型管理"],
            ["/restore", KeyRound, "恢复文件"],
            ["/integration", Cable, "AI 工具接入"],
            ["/settings", Settings2, "应用设置"],
          ].map(([path, Icon, label]) => {
            const I = Icon as typeof FileText;
            return (
              <NavLink
                key={String(path)}
                to={String(path)}
                end
                title={sidebarCollapsed ? String(label) : undefined}
                aria-label={String(label)}
              >
                <I size={19} />
                <span className="sidebar-label">{String(label)}</span>
              </NavLink>
            );
          })}
        </nav>
        <div
          className="local"
          title={sidebarCollapsed ? "模型与本机处理状态" : undefined}
        >
          <span className={model.data?.ready ? "dot ready" : "dot"} />
          <span className="sidebar-label">
            {preparing
              ? "正在准备本机模型"
              : model.data?.ready
                ? "基础模型已就绪"
                : "模型需要处理"}
            <small>所有内容仅在本机处理</small>
          </span>
        </div>
        <div
          className="account-summary"
          title={
            sidebarCollapsed
              ? `${auth.subject?.display_name || auth.subject?.username} · ${auth.subject?.tenant_name}`
              : undefined
          }
        >
          <UserRound size={18} />
          <span className="sidebar-label">
            {auth.subject?.display_name || auth.subject?.username}
            <small>
              {auth.subject?.tenant_name}
              {auth.offline ? " · 离线" : ""}
            </small>
          </span>
          <button
            type="button"
            className="account-logout"
            aria-label="退出登录"
            title="退出登录"
            onClick={() =>
              void onLogout().catch((error) =>
                setNavigationError(message(error)),
              )
            }
          >
            <LogOut size={16} />
          </button>
        </div>
        <button
          type="button"
          className="sidebar-toggle secondary"
          aria-label={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}
          title={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}
          onClick={() =>
            usePreferences.setState({
              sidebar: sidebarCollapsed ? "expanded" : "collapsed",
            })
          }
        >
          {sidebarCollapsed ? (
            <PanelLeftOpen size={18} />
          ) : (
            <PanelLeftClose size={18} />
          )}
          <span className="sidebar-label">折叠侧边栏</span>
        </button>
      </aside>
      <main className={inActiveWorkbench ? "active-workbench-main" : ""}>
        {auth.offline && (
          <div className="offline-banner" role="status">
            <CircleAlert size={17} />
            <span>认证服务暂时不可用，当前处于离线使用期</span>
            {auth.offline_until && (
              <strong>
                可用至{" "}
                {new Date(auth.offline_until * 1000).toLocaleString("zh-CN")}
              </strong>
            )}
          </div>
        )}
        {activity && (
          <div
            className={`global-progress ${activity.terminal ? "terminal" : ""} ${activity.failed ? "failed" : ""}`}
            role={activity.failed ? "alert" : "status"}
            aria-live="polite"
          >
            {activity.terminal ? (
              activity.failed ? (
                <CircleAlert size={17} />
              ) : (
                <ShieldCheck size={17} />
              )
            ) : (
              <LoaderCircle size={17} className="spin" />
            )}
            <span>{activity.stage}</span>
            {activity.total ? (
              <strong>
                {activity.done ?? 0} / {activity.total}
              </strong>
            ) : (
              <span>请稍候…</span>
            )}
          </div>
        )}
        {prepareError && (
          <div className="setup-banner" role="alert">
            <CircleAlert size={18} />
            <span>模型准备失败：{prepareError}</span>
            <NavLink to="/models">查看并重试</NavLink>
          </div>
        )}
        {navigationError && (
          <p className="error" role="alert">
            {navigationError}
            <button
              className="secondary"
              onClick={() =>
                void retryReview()
                  .then(() => setNavigationError(""))
                  .catch((e) => setNavigationError(message(e)))
              }
            >
              重试保存
            </button>
          </p>
        )}
        {preparing && location.pathname !== "/settings" ? (
          <section className="setup-page" aria-live="polite">
            <LoaderCircle size={34} className="spin" />
            <h1>正在准备本机识别能力</h1>
            <p>
              首次使用需要下载并校验基础模型。完成后即可开始处理，文件内容不会上传。
            </p>
            <progress max="100" value={setupProgress?.percent} />
            <strong>{setupProgress?.message ?? "正在检查已安装模型…"}</strong>
            {modelProgressDetails(setupProgress ?? {}) && (
              <span className="setup-progress-meta">
                {modelProgressDetails(setupProgress ?? {})}
              </span>
            )}
            <small>
              {setupProgress.source_label
                ? `当前来源：${setupProgress.source_label}`
                : "优先使用阿里云 OSS"}
              {" · 下载失败时自动切换 ModelScope · 支持断点续传"}
            </small>
          </section>
        ) : (
          <Routes>
            <Route path="/" element={<Workbench />} />
            <Route path="/history" element={<Tasks />} />
            <Route path="/batch" element={<Batch />} />
            <Route path="/rules" element={<Rules />} />
            <Route path="/policies" element={<Policies />} />
            <Route path="/models" element={<Models />} />
            <Route path="/restore" element={<Restore />} />
            <Route path="/integration" element={<Integration />} />
            <Route path="/settings" element={<Settings />} />
          </Routes>
        )}
      </main>
    </div>
  );
}
function modelStageLabel(stage: string): string {
  return (
    (
      {
        verifying: "正在校验模型",
        connecting: "正在连接模型下载源",
        switching_source: "正在切换备用下载源",
        retrying: "正在重新下载模型",
        downloading: "正在下载模型",
        installing: "正在安装模型",
        loading: "正在加载模型",
        failed: "模型准备失败",
      } as Record<string, string>
    )[stage] ?? "正在准备模型"
  );
}
function taskStageLabel(stage: string): string {
  return (
    (
      {
        queued: "任务已加入队列",
        analyzing: "正在识别敏感内容",
        processing: "正在生成脱敏文件",
        validating: "正在校验输出",
        exporting: "正在导出文件",
        awaiting_review: "识别完成，请复核结果",
        completed: "脱敏文件已生成",
        partial: "任务已完成，部分内容需要检查",
        failed: "任务处理失败",
        cancelled: "任务已取消",
      } as Record<string, string>
    )[stage] ?? "正在处理任务"
  );
}
function batchStageLabel(stage: string): string {
  return (
    (
      {
        completed: "批量处理已完成",
        partial: "批量处理完成，部分文件失败",
        failed: "批量处理失败",
        cancelled: "批量处理已取消",
      } as Record<string, string>
    )[stage] ?? "批量处理中"
  );
}
function Heading({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <header>
      <h1>{title}</h1>
      <p>{children}</p>
    </header>
  );
}
function Integration() {
  const integration = useQuery({
    queryKey: ["integration"],
    queryFn: () => call("integration_info"),
  });
  const action = useAction();
  const info = integration.data;
  const configuration =
    info?.enabled && info.executable_path
      ? JSON.stringify(
          {
            mcpServers: {
              sixa: {
                command: info.executable_path,
                args: ["serve"],
              },
            },
          },
          null,
          2,
        )
      : "";

  const copy = (value: string, label: string) =>
    action.run(async () => {
      if (!navigator.clipboard?.writeText) {
        throw new Error("当前环境无法访问剪贴板");
      }
      await navigator.clipboard.writeText(value);
      action.setNotice(`${label}已复制`);
    });

  return (
    <>
      <Heading title="AI 工具接入">
        让支持 MCP 的本机 AI 工具安全调用私匣处理文件。
      </Heading>
      <Feedback
        {...action}
        error={
          action.error || (integration.error ? message(integration.error) : "")
        }
      />
      {integration.isPending ? (
        <section className="integration-loading" aria-live="polite">
          <LoaderCircle size={20} className="spin" />
          正在读取接入信息
        </section>
      ) : info ? (
        <div className="integration-layout">
          <section
            className="card integration-status"
            aria-labelledby="integration-status-title"
          >
            <div className="toolbar">
              <h2 id="integration-status-title">接入状态</h2>
              <span className={`badge ${info.enabled ? "" : "warning-badge"}`}>
                {info.enabled ? "默认开启" : "仅支持 Windows"}
              </span>
            </div>
            <div className="integration-checks">
              <StatusRow
                ready={info.authenticated}
                label="桌面会话"
                detail={
                  info.authenticated
                    ? "已登录，可接受任务"
                    : "需要先登录桌面应用"
                }
              />
              <StatusRow
                ready={info.mcp_available}
                label="MCP 程序"
                detail={
                  !info.enabled
                    ? "当前平台暂不提供"
                    : info.mcp_available
                      ? "已随桌面应用安装"
                      : "未找到接入程序"
                }
              />
              <StatusRow
                ready={info.models_ready}
                label="本机模型"
                detail={
                  info.models_ready ? "已就绪" : "需要在模型管理中完成准备"
                }
              />
            </div>
            <button
              type="button"
              className="secondary"
              disabled={action.busy || !info.enabled}
              onClick={() =>
                action.run(async () => {
                  const result = await call("integration_check");
                  if (!result.ok) throw new Error(result.message);
                  action.setNotice(result.message || "本机连接正常");
                  await integration.refetch();
                })
              }
            >
              <RefreshCw
                size={17}
                className={action.busy ? "spin" : undefined}
              />
              本机连接自检
            </button>
          </section>

          <section
            className="card integration-config"
            aria-labelledby="integration-config-title"
          >
            <div className="toolbar">
              <h2 id="integration-config-title">MCP 配置</h2>
              <button
                type="button"
                className="secondary"
                disabled={action.busy || !configuration}
                onClick={() => void copy(configuration, "MCP 配置")}
              >
                <Copy size={16} />
                复制配置
              </button>
            </div>
            {info.enabled ? (
              <>
                <p>
                  私匣在本机脱敏 PDF、Office、图片和文本文件，只向 AI
                  返回任务状态与结果路径，不返回文件正文。将这段配置添加到 AI
                  工具后，请保持桌面应用运行并已登录。
                </p>
                <pre
                  className="integration-code"
                  aria-label="通用 MCP JSON 配置"
                >
                  {configuration}
                </pre>
              </>
            ) : (
              <p>
                当前 macOS 版本不提供 MCP
                sidecar；桌面文件脱敏功能可以正常使用。
              </p>
            )}
          </section>

          <section
            className="card integration-details"
            aria-labelledby="integration-details-title"
          >
            <h2 id="integration-details-title">程序信息</h2>
            <dl>
              <dt>MCP 程序路径</dt>
              <dd>
                <code>{info.executable_path || "当前平台未提供"}</code>
                <button
                  type="button"
                  className="secondary icon-button"
                  aria-label="复制 MCP 程序路径"
                  title="复制 MCP 程序路径"
                  disabled={action.busy || !info.executable_path}
                  onClick={() => void copy(info.executable_path, "程序路径")}
                >
                  <Copy size={16} />
                </button>
              </dd>
              <dt>协议版本</dt>
              <dd>{info.protocol_version}</dd>
              <dt>支持格式</dt>
              <dd>{info.supported_formats.join("、")}</dd>
              <dt>调用流程</dt>
              <dd>
                {info.enabled
                  ? "检查状态 → 创建任务 → 等待终态 → 获取结果路径"
                  : "当前平台暂不提供 MCP 调用"}
              </dd>
            </dl>
          </section>
        </div>
      ) : null}
    </>
  );
}

function StatusRow({
  ready,
  label,
  detail,
}: {
  ready: boolean;
  label: string;
  detail: string;
}) {
  return (
    <div className={`integration-status-row ${ready ? "ready" : ""}`}>
      {ready ? <CheckCircle2 size={19} /> : <XCircle size={19} />}
      <strong>{label}</strong>
      <span>{detail}</span>
    </div>
  );
}

function Highlight({
  text,
  entities,
  focusedId,
  onFocus,
}: {
  text: string;
  entities: Entity[];
  focusedId?: string | null;
  onFocus?: (id: string) => void;
}) {
  let cursor = 0;
  const parts: React.ReactNode[] = [];
  for (const e of [...entities].sort(
    (a, b) => a.display.start - b.display.start,
  )) {
    parts.push(text.slice(cursor, e.display.start));
    parts.push(
      <mark
        data-entity-id={e.id}
        key={e.id}
        className={`${e.selected ? "" : "unselected"} ${focusedId === e.id ? "focused" : ""}`}
        title={e.type_label}
        role="button"
        tabIndex={0}
        onClick={() => onFocus?.(e.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onFocus?.(e.id);
          }
        }}
      >
        {text.slice(e.display.start, e.display.end)}
      </mark>,
    );
    cursor = e.display.end;
  }
  parts.push(text.slice(cursor));
  return <pre>{parts}</pre>;
}
const documentPreviewCache = new Map<string, Promise<DocumentPreview>>();

function sourcePreview(id: string, reload = false) {
  if (reload) documentPreviewCache.delete(id);
  const cached = documentPreviewCache.get(id);
  if (cached) {
    documentPreviewCache.delete(id);
    documentPreviewCache.set(id, cached);
    return cached;
  }
  const request = call("document_manifest", { id, result: false });
  documentPreviewCache.set(id, request);
  void request.catch(() => {
    if (documentPreviewCache.get(id) === request)
      documentPreviewCache.delete(id);
  });
  while (documentPreviewCache.size > 3) {
    const oldest = documentPreviewCache.keys().next().value;
    if (oldest) documentPreviewCache.delete(oldest);
  }
  return request;
}

type PreviewMode = "source" | "result" | "split";
type InspectorTab = "entities" | "regions";

function VisualPreview({
  preview,
  regions,
  result,
  editable,
  showHelpers,
  zoom,
  onCreate,
  taskId,
  output = false,
  focusedId,
  onFocus,
  drawMode = false,
  onUpdate,
  currentPage = 0,
  draftRevision,
  draftReady = false,
  officeImages = false,
  fitPage = false,
}: {
  preview: DocumentPreview;
  regions: Region[];
  result: boolean;
  editable: boolean;
  showHelpers: boolean;
  zoom: number;
  onCreate?: (region: Region) => void;
  taskId: string;
  output?: boolean;
  focusedId?: string | null;
  onFocus?: (region: Region) => void;
  drawMode?: boolean;
  onUpdate?: (region: Region) => void;
  currentPage?: number;
  draftRevision?: number;
  draftReady?: boolean;
  officeImages?: boolean;
  fitPage?: boolean;
}) {
  const [moving, setMoving] = useState<{
    region: Region;
    start: { x: number; y: number };
    current: Region;
    resize: boolean;
    pointer: number;
  } | null>(null);
  const [visiblePages, setVisiblePages] = useState<Set<number>>(new Set());
  const [draft, setDraft] = useState<{
    page: number;
    pointerId: number;
    start: { x: number; y: number };
    end: { x: number; y: number };
  } | null>(null);
  const regionsByPage = useMemo(() => {
    const pages = new Map<number, Region[]>();
    for (const region of regions) {
      const items = pages.get(region.page) ?? [];
      items.push(region);
      pages.set(region.page, items);
    }
    return pages;
  }, [regions]);
  useEffect(() => {
    if (!draft && !moving) return;
    const cancel = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setDraft(null);
        setMoving(null);
      }
    };
    window.addEventListener("keydown", cancel);
    return () => window.removeEventListener("keydown", cancel);
  }, [draft, moving]);
  const point = (event: React.PointerEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    return {
      x: Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width)),
      y: Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height)),
    };
  };
  const finish = (event: React.PointerEvent<SVGSVGElement>, page: number) => {
    if (!editable || !draft || draft.page !== page || !onCreate) return;
    const end = point(event);
    const x0 = Math.min(draft.start.x, end.x);
    const y0 = Math.min(draft.start.y, end.y);
    const x1 = Math.max(draft.start.x, end.x);
    const y1 = Math.max(draft.start.y, end.y);
    if (event.currentTarget.hasPointerCapture(draft.pointerId))
      event.currentTarget.releasePointerCapture(draft.pointerId);
    setDraft(null);
    if (x1 - x0 < 0.005 || y1 - y0 < 0.005) return;
    onCreate({
      id: crypto.randomUUID(),
      page,
      polygon: [
        { x: x0, y: y0 },
        { x: x1, y: y0 },
        { x: x1, y: y1 },
        { x: x0, y: y1 },
      ],
      entity_id: null,
      selected: true,
      source: "manual",
      text: "手工区域",
      score: null,
      rotation: 0,
      replacement: "已脱敏",
    });
  };
  return (
    <div className="page-list">
      {preview.pages.map((page) => {
        const pageRegions = (
          visiblePages.has(page.index)
            ? (regionsByPage.get(page.index) ?? [])
            : []
        ).map((region) =>
          moving?.region.id === region.id ? moving.current : region,
        );
        const currentDraft = draft?.page === page.index ? draft : null;
        const trueDraft = result && !output;
        return (
          <div
            className="page-preview"
            data-page={page.index}
            key={page.index}
            style={{
              width: `${zoom * 100}%`,
              maxWidth: fitPage
                ? `min(${900 * zoom}px, calc((100vh - 255px) * ${page.width / page.height}))`
                : `${900 * zoom}px`,
            }}
          >
            <PageImage
              id={taskId}
              result={output}
              revision={
                trueDraft
                  ? (draftRevision ?? preview.revision)
                  : preview.revision
              }
              page={page}
              draft={trueDraft}
              enabled={!trueDraft || (draftReady && page.index === currentPage)}
              label={officeImages ? `内嵌图片 ${page.index + 1}` : undefined}
              maxDimension={
                zoom > 1.3 && page.index === currentPage ? 2400 : 1400
              }
              onVisible={(visible) =>
                setVisiblePages((pages) => {
                  const next = new Set(pages);
                  if (visible) next.add(page.index);
                  else next.delete(page.index);
                  return next;
                })
              }
            />
            <svg
              viewBox="0 0 1 1"
              preserveAspectRatio="none"
              className={
                editable && drawMode ? "region-layer editable" : "region-layer"
              }
              data-testid={`region-canvas-page-${page.index}`}
              onPointerDown={(event) => {
                if (!editable || !drawMode || event.button !== 0) return;
                const start = point(event);
                event.currentTarget.setPointerCapture(event.pointerId);
                setDraft({
                  page: page.index,
                  pointerId: event.pointerId,
                  start,
                  end: start,
                });
              }}
              onPointerMove={(event) => {
                if (moving && moving.pointer === event.pointerId) {
                  const end = point(event);
                  const xs = moving.region.polygon.map((p) => p.x),
                    ys = moving.region.polygon.map((p) => p.y);
                  const left = Math.min(...xs),
                    top = Math.min(...ys),
                    right = Math.max(...xs),
                    bottom = Math.max(...ys);
                  const dx = Math.max(
                    -left,
                    Math.min(1 - right, end.x - moving.start.x),
                  );
                  const dy = Math.max(
                    -top,
                    Math.min(1 - bottom, end.y - moving.start.y),
                  );
                  const polygon = moving.resize
                    ? [
                        { x: left, y: top },
                        { x: Math.max(left + 0.005, end.x), y: top },
                        {
                          x: Math.max(left + 0.005, end.x),
                          y: Math.max(top + 0.005, end.y),
                        },
                        { x: left, y: Math.max(top + 0.005, end.y) },
                      ]
                    : moving.region.polygon.map((p) => ({
                        x: p.x + dx,
                        y: p.y + dy,
                      }));
                  setMoving({
                    ...moving,
                    current: { ...moving.region, polygon },
                  });
                  return;
                }
                if (!draft || draft.pointerId !== event.pointerId) return;
                setDraft({ ...draft, end: point(event) });
              }}
              onPointerUp={(event) => {
                if (moving) {
                  if (
                    JSON.stringify(moving.region.polygon) !==
                    JSON.stringify(moving.current.polygon)
                  )
                    onUpdate?.(moving.current);
                  setMoving(null);
                  if (event.currentTarget.hasPointerCapture(event.pointerId))
                    event.currentTarget.releasePointerCapture(event.pointerId);
                } else finish(event, page.index);
              }}
              onPointerCancel={(event) => {
                if (draft?.pointerId === event.pointerId) setDraft(null);
                setMoving(null);
              }}
            >
              {pageRegions
                .filter(
                  (region) =>
                    showHelpers ||
                    region.source === "manual" ||
                    region.source === "entity",
                )
                .map((region) => (
                  <polygon
                    key={region.id}
                    data-region-id={!result ? region.id : undefined}
                    points={region.polygon
                      .map((item) => `${item.x},${item.y}`)
                      .join(" ")}
                    className={`${region.source} ${region.selected ? "selected" : ""} ${focusedId && (focusedId === region.entity_id || focusedId === region.id) ? "focused" : ""}`}
                    onPointerDown={(event) => {
                      if (!drawMode) {
                        event.stopPropagation();
                        onFocus?.(region);
                        if (editable && region.source === "manual") {
                          const svg = event.currentTarget.ownerSVGElement!;
                          svg.setPointerCapture(event.pointerId);
                          const rect = svg.getBoundingClientRect();
                          setMoving({
                            region,
                            current: region,
                            resize: false,
                            pointer: event.pointerId,
                            start: {
                              x: (event.clientX - rect.left) / rect.width,
                              y: (event.clientY - rect.top) / rect.height,
                            },
                          });
                        }
                      }
                    }}
                    vectorEffect="non-scaling-stroke"
                  />
                ))}
              {!drawMode &&
                editable &&
                pageRegions
                  .filter((r) => r.id === focusedId && r.source === "manual")
                  .map((region) => (
                    <rect
                      key={`handle-${region.id}`}
                      className="region-handle"
                      x={Math.max(...region.polygon.map((p) => p.x)) - 0.008}
                      y={Math.max(...region.polygon.map((p) => p.y)) - 0.008}
                      width={0.016}
                      height={0.016}
                      onPointerDown={(event) => {
                        event.stopPropagation();
                        const svg = event.currentTarget.ownerSVGElement!;
                        svg.setPointerCapture(event.pointerId);
                        const rect = svg.getBoundingClientRect();
                        setMoving({
                          region,
                          current: region,
                          resize: true,
                          pointer: event.pointerId,
                          start: {
                            x: (event.clientX - rect.left) / rect.width,
                            y: (event.clientY - rect.top) / rect.height,
                          },
                        });
                      }}
                    />
                  ))}
              {currentDraft && (
                <rect
                  data-testid="region-draft"
                  className="region-draft"
                  x={Math.min(currentDraft.start.x, currentDraft.end.x)}
                  y={Math.min(currentDraft.start.y, currentDraft.end.y)}
                  width={Math.abs(currentDraft.end.x - currentDraft.start.x)}
                  height={Math.abs(currentDraft.end.y - currentDraft.start.y)}
                  vectorEffect="non-scaling-stroke"
                />
              )}
            </svg>
            {result && !output && (
              <span className="draft-state">
                {draftReady && page.index === currentPage
                  ? "当前页实际效果"
                  : page.index === currentPage
                    ? "修改已标记，正在更新效果"
                    : "滚动到此页查看实际效果"}
              </span>
            )}
            <span className="page-number">
              {officeImages ? "内嵌图片" : "第"} {page.index + 1}
              {officeImages ? "" : " 页"}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function Workbench() {
  const {
    task,
    setTask,
    change,
    replaceRegions,
    undo,
    dirty,
    saving,
    saveError,
    batchId,
    setBatchId,
    pendingTaskId,
  } = useWorkbench();
  const navigate = useNavigate();
  useEffect(
    () => () => {
      void flushReview().catch(() => undefined);
    },
    [],
  );
  const [filter, setFilter] = useState("");
  const [textPreview, setTextPreview] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const autoSaving = saving > 0;
  const autoSaveError = saveError;
  const [documentPreview, setDocumentPreview] =
    useState<DocumentPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState("");
  const [resultDocumentPreview, setResultDocumentPreview] =
    useState<DocumentPreview | null>(null);
  const [resultLoading, setResultLoading] = useState(false);
  const [resultError, setResultError] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [previewMode, setPreviewMode] = useState<PreviewMode>("source");
  const [officeView, setOfficeView] = useState<"body" | "images">("body");
  const [inspectorTab, setInspectorTab] = useState<InspectorTab>("entities");
  const preferences = usePreferences();
  const inspectorOpen = preferences.inspectorOpen && !preferences.focusMode;
  const setInspectorOpen = (inspectorOpen: boolean) =>
    usePreferences.setState({ inspectorOpen });
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const [drawMode, setDrawMode] = useState(false);
  const [pageNumber, setPageNumber] = useState(1);
  const [pageInput, setPageInput] = useState<string | null>(null);
  const [exportedPath, setExportedPath] = useState("");
  const [showHelpers, setShowHelpers] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [fitPage, setFitPage] = useState(false);
  const [draftRevision, setDraftRevision] = useState<number | null>(null);
  const [settledPage, setSettledPage] = useState(-1);
  const [helperRegion, setHelperRegion] = useState<Region | null>(null);
  const [docxPageCount, setDocxPageCount] = useState(0);
  const [docxPageRequest, setDocxPageRequest] = useState<
    { page: number; token: number } | undefined
  >();
  const [docxScrollAnchor, setDocxScrollAnchor] = useState<{
    result: boolean;
    value: DocxScrollAnchor;
  } | null>(null);
  const [pendingRegions, setPendingRegions] = useState<Set<string>>(new Set());
  const [deletedRegion, setDeletedRegion] = useState<Region | null>(null);
  const undoDeleteTimer = useRef<number | null>(null);
  const sourcePaneRef = useRef<HTMLDivElement>(null);
  const resultPaneRef = useRef<HTMLDivElement>(null);
  const splitScrollLock = useRef(false);
  const canvasRef = useRef<HTMLDivElement>(null);
  const [options, setOptions] = useState<TaskOptions>({
    ocr_profile: "mobile",
    pdf_mode: "safe_rebuild",
  });
  const action = useAction();
  const working = action.busy || !!pendingTaskId;
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  const defaults = useQuery({
    queryKey: ["settings"],
    queryFn: () => call("get_settings"),
  });
  const batch = useQuery({
    queryKey: ["batch", batchId],
    queryFn: () => call("batch_view", { id: batchId! }),
    enabled: !!batchId,
  });
  useEffect(() => {
    if (defaults.data && !task)
      setOptions({
        ocr_profile: defaults.data.ocr_profile,
        pdf_mode: defaults.data.pdf_mode,
      });
  }, [defaults.data, !!task]);
  const reviewing = task?.meta.state === "awaiting_review";
  const mobileReady = capabilityInstalled(model.data, "ppocrv4-mobile-v1");
  const accurateReady = capabilityInstalled(model.data, "ppocrv4-accurate-v1");
  const taskId = task?.meta.id ?? null;
  const taskState = task?.meta.state;
  const entityById = useMemo(
    () => new Map(task?.entities.map((entity) => [entity.id, entity]) ?? []),
    [task?.entities],
  );
  const regionsByEntity = useMemo(() => {
    const result = new Map<string, Region[]>();
    for (const region of task?.regions ?? []) {
      if (!region.entity_id) continue;
      const items = result.get(region.entity_id) ?? [];
      items.push(region);
      result.set(region.entity_id, items);
    }
    return result;
  }, [task?.regions]);
  const liveRegions = useMemo(
    () =>
      (task?.regions ?? []).map((region) => {
        const entity = region.entity_id
          ? entityById.get(region.entity_id)
          : null;
        return entity
          ? {
              ...region,
              selected: entity.selected,
              replacement:
                entity.replacement ??
                entity.effective_replacement ??
                region.replacement,
            }
          : region;
      }),
    [task?.regions, entityById],
  );
  useEffect(() => {
    setFocusedId(null);
    setFilter("");
    setDrawMode(false);
    setHelperRegion(null);
    setPageNumber(1);
    setPageInput(null);
    setOfficeView("body");
    setDocxPageCount(0);
    setDocxPageRequest(undefined);
    setDocxScrollAnchor(null);
    setPreviewMode("source");
    setDeletedRegion(null);
    setExportedPath("");
    setPendingRegions(new Set());
    canvasRef.current?.scrollTo({ top: 0, left: 0 });
    sourcePaneRef.current?.scrollTo({ top: 0, left: 0 });
    resultPaneRef.current?.scrollTo({ top: 0, left: 0 });
  }, [taskId]);
  useEffect(() => {
    setDraftRevision(null);
    if (
      !task ||
      !reviewing ||
      dirty ||
      autoSaving ||
      pendingRegions.size ||
      autoSaveError ||
      previewMode === "source"
    )
      return;
    const timer = window.setTimeout(() => setDraftRevision(task.revision), 500);
    return () => window.clearTimeout(timer);
  }, [
    taskId,
    task?.revision,
    reviewing,
    dirty,
    autoSaving,
    pendingRegions.size,
    autoSaveError,
    previewMode,
  ]);
  useEffect(() => {
    setSettledPage(-1);
    if (previewMode === "source") return;
    const timer = window.setTimeout(() => setSettledPage(pageNumber - 1), 160);
    return () => window.clearTimeout(timer);
  }, [pageNumber, previewMode, taskId]);

  const jumpPage = (page: number) => {
    setPageNumber(page + 1);
    if (task?.extension === "docx" && officeView === "body") {
      setDocxPageRequest((current) => ({
        page,
        token: (current?.token ?? 0) + 1,
      }));
      return;
    }
    canvasRef.current
      ?.querySelectorAll(`[data-page="${page}"]`)
      .forEach((element) =>
        element.scrollIntoView({ block: "start", behavior: "smooth" }),
      );
  };
  const centerLocation = (id: string, region?: Region) => {
    window.requestAnimationFrame(() =>
      window.requestAnimationFrame(() => {
        const anchor = canvasRef.current?.querySelector(
          `[data-entity-id="${id}"]`,
        );
        if (anchor)
          anchor.scrollIntoView({
            block: "center",
            inline: "center",
            behavior: "smooth",
          });
        else if (region) {
          const page = canvasRef.current?.querySelector<HTMLElement>(
            `[data-page="${region.page}"]`,
          );
          const scroll = page?.closest<HTMLElement>(
            ".preview-pane, .canvas-scroll",
          );
          if (page && scroll) {
            const pageRect = page.getBoundingClientRect(),
              scrollRect = scroll.getBoundingClientRect();
            const y =
              region.polygon.reduce((sum, p) => sum + p.y, 0) /
              region.polygon.length;
            scroll.scrollBy({
              top:
                pageRect.top -
                scrollRect.top +
                y * pageRect.height -
                scroll.clientHeight / 2,
              behavior: "smooth",
            });
          }
        }
      }),
    );
  };
  const focusEntity = (id: string) => {
    setFocusedId(id);
    setHelperRegion(null);
    setInspectorTab("entities");
    if (!preferences.focusMode) setInspectorOpen(true);
    const region = regionsByEntity.get(id)?.[0];
    if (
      task?.meta.state === "completed" ||
      (!region && task?.extension !== "docx")
    )
      setPreviewMode("source");
    if (["docx", "xlsx", "xlsm"].includes(task?.extension ?? ""))
      setOfficeView(region ? "images" : "body");
    if (region) setPageNumber(region.page + 1);
    centerLocation(id, region);
  };
  const matchesEntity = (entity: Entity) =>
    (entity.text + " " + entity.type_label)
      .toLocaleLowerCase()
      .includes(filter.trim().toLocaleLowerCase());

  useEffect(() => {
    if (!taskId || pageNumber <= 1) return;
    const frame = window.requestAnimationFrame(() => jumpPage(pageNumber - 1));
    return () => window.cancelAnimationFrame(frame);
  }, [previewMode]);

  useEffect(() => {
    if (!taskId) {
      setDocumentPreview(null);
      setResultDocumentPreview(null);
      setTextPreview(null);
      return;
    }
    let active = true;
    setPreviewLoading(true);
    setPreviewError("");
    setDocumentPreview(null);
    sourcePreview(taskId)
      .then((value) => active && setDocumentPreview(value))
      .catch((error) => active && setPreviewError(message(error)))
      .finally(() => active && setPreviewLoading(false));
    return () => {
      active = false;
    };
  }, [taskId]);

  useEffect(() => {
    if (!taskId || taskState !== "completed") {
      setResultDocumentPreview(null);
      return;
    }
    let active = true;
    setResultLoading(true);
    setResultError("");
    call("document_manifest", { id: taskId, result: true })
      .then((value) => active && setResultDocumentPreview(value))
      .catch((error) => active && setResultError(message(error)))
      .finally(() => active && setResultLoading(false));
    return () => {
      active = false;
    };
  }, [taskId, taskState]);

  useEffect(() => {
    if (
      !taskId ||
      !documentPreview ||
      task?.extension === "docx" ||
      (officeView === "images" &&
        ["xlsx", "xlsm"].includes(task?.extension ?? "")) ||
      (documentPreview.pages.length > 0 &&
        !["docx", "xlsx", "xlsm"].includes(task?.extension ?? "")) ||
      previewMode === "source"
    )
      return;
    let active = true;
    setResultLoading(true);
    call("preview", { id: taskId })
      .then((value) => active && setTextPreview(value))
      .catch((error) => active && setPreviewError(message(error)))
      .finally(() => active && setResultLoading(false));
    return () => {
      active = false;
    };
  }, [documentPreview, task?.revision, taskId, previewMode, officeView]);

  useEffect(
    () => () => {
      if (undoDeleteTimer.current !== null)
        window.clearTimeout(undoDeleteTimer.current);
    },
    [],
  );

  const trackPage = (element: HTMLDivElement) => {
    const top =
      element.getBoundingClientRect().top +
      Math.min(160, element.clientHeight / 3);
    const pages = [...element.querySelectorAll<HTMLElement>("[data-page]")];
    const current = pages.find(
      (page) => page.getBoundingClientRect().bottom > top,
    );
    if (current) setPageNumber(Number(current.dataset.page) + 1);
  };
  const syncSplitScroll = (
    event: React.UIEvent<HTMLDivElement>,
    peer: React.RefObject<HTMLDivElement | null>,
  ) => {
    trackPage(event.currentTarget);
    if (splitScrollLock.current || !peer.current) return;
    const source = event.currentTarget;
    const target = peer.current;
    const horizontalRange = source.scrollWidth - source.clientWidth;
    splitScrollLock.current = true;
    const sourceTop = source.getBoundingClientRect().top;
    const anchors = [
      ...source.querySelectorAll<HTMLElement>(
        "[data-content-anchor], [data-page]",
      ),
    ];
    const anchor = anchors.find(
      (item) => item.getBoundingClientRect().bottom > sourceTop + 50,
    );
    if (anchor) {
      const attribute = anchor.dataset.contentAnchor
        ? "data-content-anchor"
        : "data-page";
      const value = anchor.getAttribute(attribute)!;
      const peerAnchor = [
        ...target.querySelectorAll<HTMLElement>(`[${attribute}]`),
      ].find((item) => item.getAttribute(attribute) === value);
      if (peerAnchor) {
        const fraction =
          (sourceTop + 50 - anchor.getBoundingClientRect().top) /
          Math.max(1, anchor.getBoundingClientRect().height);
        target.scrollTop +=
          peerAnchor.getBoundingClientRect().top -
          target.getBoundingClientRect().top +
          fraction * peerAnchor.getBoundingClientRect().height -
          50;
      }
    }
    target.scrollLeft =
      horizontalRange > 0
        ? (source.scrollLeft / horizontalRange) *
          Math.max(0, target.scrollWidth - target.clientWidth)
        : 0;
    window.requestAnimationFrame(() => {
      splitScrollLock.current = false;
    });
  };

  const queueRegionMutation = (regionId: string, region: Region | string) => {
    if (!taskId) return;
    setPendingRegions((current) => new Set(current).add(regionId));
    void saveRegion(taskId, region)
      .catch(() => undefined)
      .finally(() =>
        setPendingRegions((current) => {
          const next = new Set(current);
          next.delete(regionId);
          return next;
        }),
      );
  };
  const addRegion = (region: Region) => {
    if (!task) return;
    replaceRegions([
      ...task.regions.filter((item) => item.id !== region.id),
      region,
    ]);
    setInspectorTab("regions");
    setInspectorOpen(true);
    usePreferences.setState({ focusMode: false });
    setFocusedId(region.id);
    setHelperRegion(null);
    queueRegionMutation(region.id, region);
    window.requestAnimationFrame(() =>
      document.getElementById(`replacement-${region.id}`)?.focus(),
    );
  };
  const editRegion = (region: Region) => {
    const current = useWorkbench.getState().task;
    if (!current) return;
    replaceRegions(
      current.regions.map((item) => (item.id === region.id ? region : item)),
    );
    queueRegionMutation(region.id, region);
  };

  const removeRegion = (region: Region) => {
    if (!task) return;
    replaceRegions(task.regions.filter((item) => item.id !== region.id));
    setDeletedRegion(region);
    if (undoDeleteTimer.current !== null)
      window.clearTimeout(undoDeleteTimer.current);
    undoDeleteTimer.current = window.setTimeout(
      () => setDeletedRegion(null),
      5000,
    );
    queueRegionMutation(region.id, region.id);
  };

  const restoreDeletedRegion = () => {
    if (!deletedRegion) return;
    if (undoDeleteTimer.current !== null)
      window.clearTimeout(undoDeleteTimer.current);
    const region = deletedRegion;
    setDeletedRegion(null);
    addRegion(region);
  };

  useEffect(() => {
    if (!task || !dirty || !reviewing) return;
    const timer = window.setTimeout(
      () => void saveEntities().catch(() => undefined),
      650,
    );
    return () => window.clearTimeout(timer);
  }, [dirty, reviewing, task?.entities, task?.revision, taskId]);
  const sync = async () => {
    await flushReview();
    return useWorkbench.getState().task!;
  };

  const choose = () =>
    action.run(async () => {
      await flushReview();
      const path = await open({
        multiple: false,
        filters: [
          {
            name: "支持的文件",
            extensions: [
              "txt",
              "md",
              "png",
              "jpg",
              "jpeg",
              "bmp",
              "tif",
              "tiff",
              "pdf",
              "docx",
              "xlsx",
              "xlsm",
            ],
          },
        ],
      });
      if (typeof path !== "string") return;
      const extension = path.split(".").pop()?.toLowerCase() ?? "";
      const needsOcr = [
        "png",
        "jpg",
        "jpeg",
        "bmp",
        "tif",
        "tiff",
        "pdf",
      ].includes(extension);
      const selectedOcrReady =
        options.ocr_profile === "accurate" ? accurateReady : mobileReady;
      if (needsOcr && !selectedOcrReady)
        throw Error(
          `${options.ocr_profile === "accurate" ? "高精度" : "轻量"} OCR 尚未安装，请先前往模型管理安装。`,
        );
      const requestId = crypto.randomUUID();
      setPendingId(requestId);
      useWorkbench.setState({ pendingTaskId: requestId });
      try {
        const analyzed = await call("analyze_file", {
          path,
          options,
          requestId,
        });
        if (useWorkbench.getState().pendingTaskId === requestId)
          setTask(analyzed);
      } finally {
        setPendingId(null);
        if (useWorkbench.getState().pendingTaskId === requestId)
          useWorkbench.setState({ pendingTaskId: null });
      }
    });

  const exportFile = () =>
    action.run(async () => {
      if (!task) return;
      const path = await save({
        defaultPath: exportName(task.meta.display_name, task.extension),
      });
      if (path) {
        await call("export_task", { id: task.meta.id, path });
        rememberDirectory(path);
        setExportedPath(path);
      }
    });

  const batchItems =
    batch.data?.items.filter(
      (item) =>
        item.task_id && ["awaiting_review", "completed"].includes(item.state),
    ) ?? [];
  const batchIndex = batchItems.findIndex((item) => item.task_id === taskId);
  const stepBatch = (direction: number, confirm = false) =>
    action.run(async () => {
      await flushReview();
      const current = useWorkbench.getState().task;
      if (!current || !batchId) return;
      if (confirm && current.meta.state === "awaiting_review") {
        useWorkbench.getState().markSaved(
          await call("confirm_review", {
            id: current.meta.id,
            expectedRevision: current.revision,
          }),
        );
        await batch.refetch();
      }
      const next = batchItems[batchIndex + direction];
      if (next?.task_id) setTask(await call("task_view", { id: next.task_id }));
      else navigate(`/batch?id=${batchId}`);
    });

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape" && usePreferences.getState().focusMode)
        usePreferences.setState({ focusMode: false });
      if (!(event.ctrlKey || event.metaKey)) return;
      const input =
        event.target instanceof HTMLElement &&
        !!event.target.closest("input, textarea, [contenteditable]");
      if (event.key.toLowerCase() === "s") {
        event.preventDefault();
        void flushReview().catch((e) => action.setError(message(e)));
      }
      if (event.key.toLowerCase() === "z" && !input && reviewing) {
        event.preventDefault();
        void undoReview().catch((e) => action.setError(message(e)));
      }
      if (event.key.toLowerCase() === "f" && !input) {
        event.preventDefault();
        setInspectorOpen(true);
        setInspectorTab("entities");
        window.setTimeout(
          () =>
            document
              .querySelector<HTMLInputElement>('[aria-label="筛选实体"]')
              ?.focus(),
          0,
        );
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [reviewing]);

  if (!task) {
    return (
      <>
        <Feedback {...action} />
        <section className="card entry file-entry">
          <FolderOpen size={42} />
          <h1>选择需要脱敏的文件</h1>
          <p>支持 TXT、Markdown、图片、多页 TIFF、PDF、DOCX、XLSX 和 XLSM。</p>
          <details className="advanced-options">
            <summary>
              <Settings2 size={16} />
              高级设置
            </summary>
            <div className="option-row">
              <label>
                图片识别
                <select
                  value={options.ocr_profile}
                  onChange={(event) =>
                    setOptions({
                      ...options,
                      ocr_profile: event.target
                        .value as TaskOptions["ocr_profile"],
                    })
                  }
                >
                  <option value="mobile" disabled={!mobileReady}>
                    轻量（速度优先）{!mobileReady ? " · 未就绪" : ""}
                  </option>
                  <option value="accurate" disabled={!accurateReady}>
                    高精度（效果优先）{!accurateReady ? " · 未安装" : ""}
                  </option>
                </select>
                <small>图片、扫描件和 PDF 需要对应 OCR 模型。</small>
              </label>
              <label>
                PDF 方式
                <select
                  value={options.pdf_mode}
                  onChange={(event) =>
                    setOptions({
                      ...options,
                      pdf_mode: event.target.value as TaskOptions["pdf_mode"],
                    })
                  }
                >
                  <option value="safe_rebuild">安全重建（推荐）</option>
                  <option value="fidelity">保真脱敏（高级）</option>
                </select>
                <small>安全重建会移除原文字层、附件和脚本。</small>
              </label>
            </div>
          </details>
          <div className="entry-actions">
            <button
              onClick={choose}
              disabled={working || !capabilityInstalled(model.data, "raner-v1")}
            >
              {working ? (
                <LoaderCircle size={17} className="spin" />
              ) : (
                <FolderOpen size={17} />
              )}
              {working ? "正在识别文件…" : "选择本机文件"}
            </button>
            {working && (pendingId || pendingTaskId) && (
              <button
                className="secondary"
                onClick={() =>
                  void call("cancel_task", {
                    id: (pendingId || pendingTaskId)!,
                  })
                }
              >
                <Square size={16} />
                取消任务
              </button>
            )}
          </div>
          {!capabilityInstalled(model.data, "raner-v1") && (
            <p className="hint">
              中文实体识别模型尚未就绪。
              <NavLink to="/models">前往模型管理</NavLink>
            </p>
          )}
        </section>
      </>
    );
  }

  const selectedCount = task.entities.filter(
    (entity) => entity.selected,
  ).length;
  const manualRegions = task.regions.filter(
    (region) => region.source === "manual",
  );
  const hasVisualPreview = !!documentPreview?.pages.length;
  const isOffice = ["docx", "xlsx", "xlsm"].includes(task.extension);
  const savedDraftReady =
    reviewing &&
    draftRevision === task.revision &&
    !dirty &&
    !autoSaving &&
    !pendingRegions.size &&
    !autoSaveError;
  const pageControls =
    hasVisualPreview && (!isOffice || officeView === "images");
  const loadResult = () => {
    setResultLoading(true);
    setResultError("");
    call("document_manifest", { id: task.meta.id, result: true })
      .then((value) => {
        if (useWorkbench.getState().task?.meta.id === task.meta.id)
          setResultDocumentPreview(value);
      })
      .catch((error) => {
        if (useWorkbench.getState().task?.meta.id === task.meta.id)
          setResultError(message(error));
      })
      .finally(() => {
        if (useWorkbench.getState().task?.meta.id === task.meta.id)
          setResultLoading(false);
      });
  };

  const visual = (result: boolean, split = false) => {
    if (
      result &&
      task.meta.state === "completed" &&
      (resultLoading || resultError || !resultDocumentPreview)
    ) {
      return (
        <div
          className={`preview-state ${resultError ? "error" : ""}`}
          role={resultError ? "alert" : "status"}
        >
          {resultError ? (
            <>
              <CircleAlert size={20} />
              <span>无法加载已生成文件：{resultError}</span>
              <button className="secondary" onClick={loadResult}>
                重新加载结果
              </button>
            </>
          ) : (
            <>
              <LoaderCircle className="spin" />
              正在加载已生成文件
            </>
          )}
        </div>
      );
    }
    if (previewLoading)
      return (
        <div className="preview-state" role="status">
          <LoaderCircle className="spin" />
          正在加载预览
        </div>
      );
    if (previewError)
      return (
        <div className="preview-state error" role="alert">
          <CircleAlert size={20} />
          <span>{previewError}</span>
          <button
            className="secondary"
            onClick={() => {
              if (!taskId) return;
              setPreviewLoading(true);
              setPreviewError("");
              sourcePreview(taskId, true)
                .then((value) => {
                  if (useWorkbench.getState().task?.meta.id === taskId)
                    setDocumentPreview(value);
                })
                .catch((error) => {
                  if (useWorkbench.getState().task?.meta.id === taskId)
                    setPreviewError(message(error));
                })
                .finally(() => {
                  if (useWorkbench.getState().task?.meta.id === taskId)
                    setPreviewLoading(false);
                });
            }}
          >
            重试
          </button>
        </div>
      );
    if (task.extension === "docx" && officeView === "body") {
      return (
        <Suspense
          fallback={
            <div className="preview-state" role="status">
              <LoaderCircle className="spin" />
              正在准备 Word 预览
            </div>
          }
        >
          <DocxPreview
            task={task}
            result={result}
            completed={task.meta.state === "completed"}
            draftReady={!!savedDraftReady}
            draftRevision={draftRevision ?? undefined}
            imagePages={documentPreview?.pages}
            focusedId={focusedId}
            onFocusEntity={focusEntity}
            onOpenImage={(index) => {
              setOfficeView("images");
              setPageNumber(index + 1);
              window.requestAnimationFrame(() =>
                window.requestAnimationFrame(() =>
                  canvasRef.current
                    ?.querySelector(`[data-page="${index}"]`)
                    ?.scrollIntoView({ block: "start" }),
                ),
              );
            }}
            zoom={zoom}
            fitMode={fitPage ? "page" : "width"}
            onPageCount={setDocxPageCount}
            pageRequest={docxPageRequest}
            onPageChange={(page) => setPageNumber(page + 1)}
            scrollAnchor={
              previewMode === "split" && docxScrollAnchor?.result !== result
                ? (docxScrollAnchor?.value ?? null)
                : null
            }
            onScrollAnchor={(anchor) => {
              if (previewMode === "split")
                setDocxScrollAnchor((current) =>
                  JSON.stringify(current?.value) === JSON.stringify(anchor)
                    ? current
                    : { result, value: anchor },
                );
            }}
          />
        </Suspense>
      );
    }
    if (pageControls && documentPreview) {
      const completedResult =
        result && resultDocumentPreview?.pages.length
          ? resultDocumentPreview
          : null;
      return (
        <VisualPreview
          taskId={task.meta.id}
          output={!!completedResult}
          focusedId={focusedId}
          onFocus={(region) => {
            if (region.entity_id) {
              focusEntity(region.entity_id);
              return;
            }
            setFocusedId(region.entity_id ?? region.id);
            setInspectorTab("regions");
            setInspectorOpen(true);
            usePreferences.setState({ focusMode: false });
            setHelperRegion(region.source === "manual" ? null : region);
            document
              .getElementById(`review-${region.entity_id ?? region.id}`)
              ?.scrollIntoView({ block: "nearest" });
          }}
          drawMode={drawMode}
          onUpdate={editRegion}
          preview={completedResult ?? documentPreview}
          regions={completedResult ? [] : liveRegions}
          result={result && !completedResult}
          editable={reviewing && !working && !split}
          showHelpers={showHelpers}
          zoom={zoom}
          onCreate={addRegion}
          currentPage={pageNumber - 1}
          draftRevision={draftRevision ?? undefined}
          draftReady={!!savedDraftReady && settledPage === pageNumber - 1}
          officeImages={isOffice}
          fitPage={fitPage}
        />
      );
    }
    if (!documentPreview)
      return (
        <div className="preview-state" role="status">
          <LoaderCircle className="spin" />
          正在加载预览
        </div>
      );
    return result ? (
      resultLoading || textPreview === null ? (
        <div className="preview-state" role="status">
          <LoaderCircle className="spin" />
          正在生成预览
        </div>
      ) : (
        <pre className="text-document">
          {task.meta.state === "completed"
            ? (resultDocumentPreview?.text ?? textPreview)
            : textPreview}
        </pre>
      )
    ) : (
      <Highlight
        text={task.text}
        entities={task.entities}
        focusedId={focusedId}
        onFocus={focusEntity}
      />
    );
  };

  return (
    <div className="task-content compact-workbench">
      <Feedback {...action} />
      <div className="task-commandbar">
        <div className="task-identity">
          {batchId && (
            <button
              className="icon-button secondary"
              aria-label="返回批次"
              title="返回批次"
              onClick={() =>
                action.run(async () => {
                  await flushReview();
                  navigate(`/batch?id=${batchId}`);
                })
              }
            >
              <ChevronLeft size={17} />
            </button>
          )}
          <div>
            <strong title={task.meta.display_name}>
              {task.meta.display_name || `未命名.${task.extension}`}
            </strong>
            <span>
              <b>{stateLabels[task.meta.state]}</b> · {selectedCount}/
              {task.entities.length} 个实体
            </span>
          </div>
        </div>
        <span
          className={`save-state ${autoSaveError ? "save-error" : ""}`}
          role="status"
        >
          {autoSaveError ? (
            "保存失败"
          ) : autoSaving || dirty || pendingRegions.size ? (
            <>
              <LoaderCircle size={14} className="spin" />
              正在保存
            </>
          ) : reviewing ? (
            "已保存"
          ) : (
            "内容已锁定"
          )}
        </span>
        <div className="command-actions">
          <button
            className="secondary"
            disabled={!reviewing || !undo.length || working}
            onClick={() =>
              void undoReview().catch((e) => action.setError(message(e)))
            }
            title="撤销修改（Ctrl/⌘ Z）"
          >
            <Undo2 size={16} />
            撤销
          </button>
          <button
            className="secondary"
            disabled={working}
            onClick={() =>
              action.run(async () => {
                await flushReview();
                setTask(null);
                setBatchId(null);
                setPassword("");
                setConfirmPassword("");
              })
            }
          >
            新建任务
          </button>
          {reviewing && (
            <button
              disabled={working}
              onClick={() =>
                action.run(async () => {
                  const id = task.meta.id;
                  await sync();
                  setPendingId(id);
                  useWorkbench.setState({ pendingTaskId: id });
                  try {
                    const result = await call("execute", { id });
                    if (useWorkbench.getState().task?.meta.id === id) {
                      setTask(result);
                      setPreviewMode("result");
                    }
                  } catch (error) {
                    try {
                      const current = await call("task_view", { id });
                      if (useWorkbench.getState().task?.meta.id === id)
                        setTask(current);
                    } catch {
                      /* Keep the original execution error. */
                    }
                    throw error;
                  } finally {
                    setPendingId(null);
                    if (useWorkbench.getState().pendingTaskId === id)
                      useWorkbench.setState({ pendingTaskId: null });
                  }
                })
              }
            >
              {working ? <LoaderCircle size={16} className="spin" /> : null}
              生成脱敏文件
            </button>
          )}
          {task.meta.state === "completed" && (
            <button disabled={working} onClick={exportFile}>
              <Download size={16} />
              保存文件
            </button>
          )}
          {["failed", "cancelled"].includes(task.meta.state) && (
            <button
              disabled={working}
              onClick={() =>
                action.run(async () => {
                  setTask(await call("retry_task", { id: task.meta.id }));
                  setBatchId(null);
                })
              }
            >
              重新分析
            </button>
          )}
          {task.meta.state === "completed" && (
            <button
              className="secondary"
              title="在副本中继续调整，保留当前结果"
              onClick={() =>
                action.run(async () => {
                  setTask(await call("clone_for_review", { id: task.meta.id }));
                  setBatchId(null);
                })
              }
            >
              继续调整
            </button>
          )}
          {task.meta.state === "completed" && (
            <button
              className="secondary"
              onClick={() => {
                const details =
                  document.querySelector<HTMLDetailsElement>(
                    ".recovery-options",
                  );
                if (details) {
                  details.open = true;
                  details.scrollIntoView({
                    block: "center",
                    behavior: "smooth",
                  });
                }
              }}
              title="保存可逆恢复包"
            >
              <KeyRound size={16} />
              恢复包
            </button>
          )}
          {working && (pendingId || pendingTaskId) && (
            <button
              className="secondary"
              onClick={() =>
                void call("cancel_task", { id: (pendingId || pendingTaskId)! })
              }
            >
              <Square size={16} />
              取消
            </button>
          )}
        </div>
      </div>

      {batchId && (
        <div className="batch-navigation">
          <span>
            批次 · 第 {batchIndex + 1} / {batchItems.length} 份
          </span>
          <button
            className="secondary"
            disabled={working || batchIndex <= 0}
            onClick={() => void stepBatch(-1)}
          >
            上一份
          </button>
          <button disabled={working} onClick={() => void stepBatch(1, true)}>
            {batchIndex === batchItems.length - 1
              ? "检查完成，返回批次"
              : "检查完成，下一份"}
          </button>
        </div>
      )}
      {autoSaveError && (
        <p role="alert" className="error">
          {autoSaveError}
          <button className="secondary" onClick={() => action.run(retryReview)}>
            重试保存
          </button>
        </p>
      )}
      {exportedPath && (
        <div className="export-notice" role="status">
          <span>文件已保存</span>
          <button
            className="text-action"
            onClick={() =>
              action.run(async () => {
                await call("reveal_file", { path: exportedPath });
              })
            }
          >
            打开所在文件夹
          </button>
        </div>
      )}

      {task.warnings.map((warning) => (
        <p className="hint-banner" key={warning}>
          {warning}
        </p>
      ))}

      <div
        className={`workspace-shell ${inspectorOpen ? "" : "inspector-closed"}`}
        style={
          {
            "--inspector-width": `${preferences.inspectorWidth}px`,
          } as React.CSSProperties
        }
      >
        <section className="document-workspace" aria-label="文档预览">
          <div className="canvas-toolbar">
            <div className="segmented" aria-label="预览方式">
              <button
                className={previewMode === "source" ? "active" : ""}
                aria-pressed={previewMode === "source"}
                onClick={() => setPreviewMode("source")}
              >
                原文标注
              </button>
              <button
                className={previewMode === "result" ? "active" : ""}
                aria-pressed={previewMode === "result"}
                onClick={() => setPreviewMode("result")}
              >
                {task.meta.state === "completed" ? "已生成文件" : "脱敏效果"}
              </button>
              <button
                className={previewMode === "split" ? "active" : ""}
                aria-pressed={previewMode === "split"}
                onClick={() => setPreviewMode("split")}
              >
                <Columns2 size={15} />
                对比
              </button>
            </div>
            {isOffice && (
              <div className="segmented" aria-label="Office 内容">
                <button
                  className={officeView === "body" ? "active" : ""}
                  aria-pressed={officeView === "body"}
                  onClick={() => {
                    setOfficeView("body");
                    setDrawMode(false);
                  }}
                >
                  {task.extension === "docx" ? "文档" : "单元格与文字"}
                </button>
                <button
                  className={officeView === "images" ? "active" : ""}
                  aria-pressed={officeView === "images"}
                  disabled={!hasVisualPreview}
                  onClick={() => {
                    setOfficeView("images");
                    setPageNumber(1);
                  }}
                >
                  内嵌图片 {documentPreview?.pages.length ?? 0}
                </button>
              </div>
            )}
            {pageControls && (
              <>
                <button
                  className={`secondary ${drawMode ? "active" : ""}`}
                  aria-pressed={drawMode}
                  disabled={!reviewing}
                  onClick={() => {
                    if (previewMode === "split") setPreviewMode("source");
                    setDrawMode((value) => !value);
                  }}
                  title={drawMode ? "返回查看模式" : "在页面上拖动添加脱敏区域"}
                >
                  {drawMode ? "完成框选" : "框选"}
                </button>
                <label className="page-jump">
                  <button
                    className="icon-button secondary"
                    aria-label="上一页"
                    disabled={pageNumber <= 1}
                    onClick={() => jumpPage(pageNumber - 2)}
                  >
                    <ChevronLeft size={15} />
                  </button>
                  <input
                    aria-label="跳转页码"
                    type="number"
                    min={1}
                    max={documentPreview?.pages.length ?? 1}
                    value={pageInput ?? pageNumber}
                    onFocus={() => setPageInput(String(pageNumber))}
                    onChange={(event) => setPageInput(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") event.currentTarget.blur();
                    }}
                    onBlur={(event) => {
                      setPageInput(null);
                      jumpPage(
                        Math.max(
                          0,
                          Math.min(
                            (documentPreview?.pages.length ?? 1) - 1,
                            Number(event.currentTarget.value) - 1,
                          ),
                        ),
                      );
                    }}
                  />{" "}
                  / {documentPreview?.pages.length}
                  <button
                    className="icon-button secondary"
                    aria-label="下一页"
                    disabled={
                      pageNumber >= (documentPreview?.pages.length ?? 1)
                    }
                    onClick={() => jumpPage(pageNumber)}
                  >
                    <ChevronRight size={15} />
                  </button>
                </label>
                <button
                  className={`icon-button secondary ${showHelpers ? "active" : ""}`}
                  aria-label="显示 OCR 辅助框"
                  aria-pressed={showHelpers}
                  title="显示 OCR 辅助框"
                  onClick={() => setShowHelpers((value) => !value)}
                >
                  <ScanLine size={17} />
                </button>
              </>
            )}
            {(pageControls ||
              (task.extension === "docx" && officeView === "body")) && (
              <>
                {task.extension === "docx" &&
                  officeView === "body" &&
                  docxPageCount > 0 && (
                    <label className="page-jump">
                      <button
                        className="icon-button secondary"
                        aria-label="上一页"
                        disabled={pageNumber <= 1}
                        onClick={() => jumpPage(pageNumber - 2)}
                      >
                        <ChevronLeft size={15} />
                      </button>
                      <input
                        aria-label="跳转页码"
                        type="number"
                        min={1}
                        max={docxPageCount}
                        value={pageInput ?? pageNumber}
                        onFocus={() => setPageInput(String(pageNumber))}
                        onChange={(event) => setPageInput(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") event.currentTarget.blur();
                        }}
                        onBlur={(event) => {
                          setPageInput(null);
                          jumpPage(
                            Math.max(
                              0,
                              Math.min(
                                docxPageCount - 1,
                                Number(event.currentTarget.value) - 1,
                              ),
                            ),
                          );
                        }}
                      />{" "}
                      / {docxPageCount}
                      <button
                        className="icon-button secondary"
                        aria-label="下一页"
                        disabled={pageNumber >= docxPageCount}
                        onClick={() => jumpPage(pageNumber)}
                      >
                        <ChevronRight size={15} />
                      </button>
                    </label>
                  )}
                <div className="zoom-controls">
                  <button
                    className="icon-button secondary"
                    aria-label="缩小"
                    title="缩小"
                    disabled={zoom <= 0.5}
                    onClick={() => {
                      setFitPage(false);
                      setZoom((value) => Math.max(0.5, value - 0.25));
                    }}
                  >
                    <ZoomOut size={17} />
                  </button>
                  <button
                    className="zoom-value secondary"
                    onClick={() => {
                      setZoom(1);
                      setFitPage(false);
                    }}
                    title="适合宽度"
                  >
                    {Math.round(zoom * 100)}%
                  </button>
                  <button
                    className="icon-button secondary"
                    aria-label="放大"
                    title="放大"
                    disabled={zoom >= 3}
                    onClick={() => {
                      setFitPage(false);
                      setZoom((value) => Math.min(3, value + 0.25));
                    }}
                  >
                    <ZoomIn size={17} />
                  </button>
                </div>
                {(pageControls ||
                  (task.extension === "docx" && officeView === "body")) && (
                  <button
                    className="secondary"
                    aria-pressed={fitPage}
                    onClick={() => {
                      setZoom(1);
                      setFitPage((value) => !value);
                    }}
                  >
                    {fitPage ? "适合宽度" : "整页"}
                  </button>
                )}
              </>
            )}
            <button
              className="icon-button secondary"
              aria-label={preferences.focusMode ? "退出专注模式" : "专注模式"}
              title={
                preferences.focusMode
                  ? "退出专注模式"
                  : "收起两侧面板，专注查看文档"
              }
              onClick={() =>
                usePreferences.setState({ focusMode: !preferences.focusMode })
              }
            >
              {preferences.focusMode ? (
                <Minimize2 size={17} />
              ) : (
                <Maximize2 size={17} />
              )}
            </button>
            {!inspectorOpen && (
              <button
                className="secondary inspector-open"
                onClick={() => {
                  usePreferences.setState({ focusMode: false });
                  setInspectorOpen(true);
                }}
              >
                <ChevronsLeft size={16} />
                打开检查器
              </button>
            )}
          </div>
          <div
            ref={canvasRef}
            className={`canvas-scroll ${previewMode === "split" ? "split-preview" : ""}`}
            style={
              {
                "--compare-ratio": `${preferences.compareRatio}%`,
              } as React.CSSProperties
            }
            onScroll={(event) => {
              if (event.target === event.currentTarget)
                trackPage(event.currentTarget);
            }}
          >
            {previewMode === "split" ? (
              <>
                <div
                  className="preview-pane"
                  ref={sourcePaneRef}
                  onScroll={(event) => syncSplitScroll(event, resultPaneRef)}
                >
                  <h2>原件</h2>
                  {visual(false, true)}
                </div>
                <div
                  className="compare-resize"
                  role="separator"
                  aria-label="调整对比宽度"
                  aria-orientation="vertical"
                  tabIndex={0}
                  onKeyDown={(event) => {
                    if (["ArrowLeft", "ArrowRight"].includes(event.key)) {
                      event.preventDefault();
                      usePreferences.setState({
                        compareRatio: Math.max(
                          25,
                          Math.min(
                            75,
                            preferences.compareRatio +
                              (event.key === "ArrowLeft" ? -5 : 5),
                          ),
                        ),
                      });
                    }
                  }}
                  onPointerDown={(event) =>
                    event.currentTarget.setPointerCapture(event.pointerId)
                  }
                  onPointerMove={(event) => {
                    if (
                      event.currentTarget.hasPointerCapture(event.pointerId)
                    ) {
                      const rect = canvasRef.current!.getBoundingClientRect();
                      usePreferences.setState({
                        compareRatio: Math.max(
                          25,
                          Math.min(
                            75,
                            ((event.clientX - rect.left) / rect.width) * 100,
                          ),
                        ),
                      });
                    }
                  }}
                  onPointerUp={(event) =>
                    event.currentTarget.releasePointerCapture(event.pointerId)
                  }
                />
                <div
                  className="preview-pane"
                  ref={resultPaneRef}
                  onScroll={(event) => syncSplitScroll(event, sourcePaneRef)}
                >
                  <h2>
                    {task.meta.state === "completed"
                      ? "已生成文件"
                      : "脱敏效果"}
                  </h2>
                  {visual(true, true)}
                </div>
              </>
            ) : (
              visual(previewMode === "result")
            )}
          </div>
        </section>

        {inspectorOpen && (
          <aside className="review-inspector" aria-label="复核检查器">
            <div
              className="inspector-resize"
              role="separator"
              aria-label="调整复核面板宽度"
              aria-orientation="vertical"
              tabIndex={0}
              onKeyDown={(event) => {
                if (event.key === "ArrowLeft" || event.key === "ArrowRight")
                  usePreferences.setState({
                    inspectorWidth: Math.max(
                      280,
                      Math.min(
                        440,
                        preferences.inspectorWidth +
                          (event.key === "ArrowLeft" ? 10 : -10),
                      ),
                    ),
                  });
              }}
              onPointerDown={(event) => {
                event.currentTarget.setPointerCapture(event.pointerId);
              }}
              onPointerMove={(event) => {
                if (event.currentTarget.hasPointerCapture(event.pointerId))
                  usePreferences.setState({
                    inspectorWidth: Math.max(
                      280,
                      Math.min(
                        440,
                        event.currentTarget.parentElement!.getBoundingClientRect()
                          .right - event.clientX,
                      ),
                    ),
                  });
              }}
              onPointerUp={(event) =>
                event.currentTarget.releasePointerCapture(event.pointerId)
              }
            />
            <div className="inspector-header">
              <div role="tablist" aria-label="复核内容">
                <button
                  role="tab"
                  aria-selected={inspectorTab === "entities"}
                  className={inspectorTab === "entities" ? "active" : ""}
                  onClick={() => setInspectorTab("entities")}
                >
                  实体复核 <b>{task.entities.length}</b>
                </button>
                <button
                  role="tab"
                  aria-selected={inspectorTab === "regions"}
                  className={inspectorTab === "regions" ? "active" : ""}
                  onClick={() => setInspectorTab("regions")}
                >
                  手工区域 <b>{manualRegions.length}</b>
                </button>
              </div>
              <button
                className="icon-button secondary"
                aria-label="收起检查器"
                title="收起检查器"
                onClick={() => setInspectorOpen(false)}
              >
                <ChevronsRight size={17} />
              </button>
            </div>
            {inspectorTab === "entities" ? (
              <div className="inspector-body" role="tabpanel">
                <div className="review-tools">
                  <input
                    aria-label="筛选实体"
                    placeholder="搜索内容或类型"
                    value={filter}
                    onChange={(event) => setFilter(event.target.value)}
                  />
                  <button
                    className="secondary"
                    disabled={!reviewing || working}
                    onClick={() =>
                      change(
                        task.entities.map((entity) => ({
                          ...entity,
                          selected: matchesEntity(entity)
                            ? true
                            : entity.selected,
                        })),
                      )
                    }
                  >
                    选择结果
                  </button>
                  <button
                    className="secondary"
                    disabled={!reviewing || working}
                    onClick={() =>
                      change(
                        task.entities.map((entity) => ({
                          ...entity,
                          selected: matchesEntity(entity)
                            ? false
                            : entity.selected,
                        })),
                      )
                    }
                  >
                    取消选择
                  </button>
                </div>
                <VirtualList
                  items={task.entities.filter(
                    (entity) =>
                      matchesEntity(entity) || entity.id === focusedId,
                  )}
                  focusedId={focusedId}
                  render={(entity) => (
                    <div
                      className={`entity-item ${focusedId === entity.id ? "focused" : ""}`}
                      id={`review-${entity.id}`}
                      key={entity.id}
                    >
                      <input
                        type="checkbox"
                        aria-label={`选择${entity.text}`}
                        checked={entity.selected}
                        disabled={!reviewing || working}
                        onChange={(event) =>
                          change(
                            task.entities.map((item) =>
                              item.id === entity.id
                                ? { ...item, selected: event.target.checked }
                                : item,
                            ),
                          )
                        }
                      />
                      <button
                        className="entity-copy entity-locate"
                        onClick={() => focusEntity(entity.id)}
                      >
                        <strong>{entity.text}</strong>
                        <span>
                          <span className="tag">{entity.type_label}</span>
                          {Math.round(entity.score * 100)}% · {entity.source}
                        </span>
                        <small className="entity-context">
                          {task.text
                            .slice(
                              Math.max(0, entity.display.start - 16),
                              Math.min(
                                task.text.length,
                                entity.display.end + 22,
                              ),
                            )
                            .replace(/\s+/g, " ")}
                        </small>
                      </button>
                      <details className="entity-replacement">
                        <summary>
                          {entity.replacement ??
                            entity.effective_replacement ??
                            regionsByEntity.get(entity.id)?.[0]?.replacement ??
                            "默认方式"}
                        </summary>
                        <input
                          aria-label={`${entity.text}的替换文字`}
                          placeholder={
                            entity.effective_replacement ?? "使用默认脱敏方式"
                          }
                          value={entity.replacement ?? ""}
                          disabled={!reviewing || working}
                          onChange={(event) =>
                            change(
                              task.entities.map((item) =>
                                item.id === entity.id
                                  ? {
                                      ...item,
                                      replacement: event.target.value || null,
                                    }
                                  : item,
                              ),
                            )
                          }
                        />
                      </details>
                    </div>
                  )}
                />
                {filter &&
                  focusedId &&
                  entityById.has(focusedId) &&
                  !matchesEntity(entityById.get(focusedId)!) && (
                    <p className="filter-focus-note">
                      已临时显示文档中选中的实体，筛选条件保持不变。
                    </p>
                  )}
                {task.entities.length > 0 && (
                  <div className="entity-navigation">
                    <span>
                      {focusedId && entityById.has(focusedId)
                        ? `${task.entities.findIndex((entity) => entity.id === focusedId) + 1} / ${task.entities.length}`
                        : `${task.entities.length} 处`}
                    </span>
                    <button
                      className="secondary"
                      disabled={
                        task.entities.findIndex(
                          (entity) => entity.id === focusedId,
                        ) <= 0
                      }
                      onClick={() => {
                        const index = task.entities.findIndex(
                          (entity) => entity.id === focusedId,
                        );
                        if (index > 0) focusEntity(task.entities[index - 1].id);
                      }}
                    >
                      上一处
                    </button>
                    <button
                      className="secondary"
                      disabled={
                        task.entities.findIndex(
                          (entity) => entity.id === focusedId,
                        ) >=
                        task.entities.length - 1
                      }
                      onClick={() => {
                        const index = task.entities.findIndex(
                          (entity) => entity.id === focusedId,
                        );
                        focusEntity(task.entities[index + 1].id);
                      }}
                    >
                      下一处
                    </button>
                  </div>
                )}
                {!!task.entities.length &&
                  !task.entities.some(
                    (entity) =>
                      matchesEntity(entity) || entity.id === focusedId,
                  ) && (
                    <p className="empty">没有匹配的实体，换个关键词试试。</p>
                  )}
                {!task.entities.length && (
                  <p className="empty">
                    未识别到实体，请根据实际内容检查识别规则。
                  </p>
                )}
              </div>
            ) : (
              <div className="inspector-body" role="tabpanel">
                {helperRegion && (
                  <div className="helper-detail">
                    <strong>识别文字</strong>
                    <p>{helperRegion.text || "此区域没有识别文字"}</p>
                    <button
                      disabled={!reviewing || working}
                      onClick={() =>
                        addRegion({
                          ...helperRegion,
                          id: crypto.randomUUID(),
                          entity_id: null,
                          source: "manual",
                          selected: true,
                          replacement: "已脱敏",
                        })
                      }
                    >
                      设为脱敏区域
                    </button>
                  </div>
                )}
                <p className="region-tip">
                  <ScanLine size={16} />
                  选择“框选”后拖动添加区域；查看模式可拖动或调整已选区域，Esc
                  取消。
                </p>
                <div className="region-list">
                  {manualRegions.map((region) => (
                    <div
                      className={`region-item ${focusedId === region.id ? "focused" : ""}`}
                      id={`review-${region.id}`}
                      key={region.id}
                    >
                      <span>
                        <button
                          className="text-action"
                          onClick={() => {
                            setFocusedId(region.id);
                            jumpPage(region.page);
                          }}
                        >
                          第 {region.page + 1} 页
                        </button>
                        <input
                          id={`replacement-${region.id}`}
                          aria-label={`第 ${region.page + 1} 页区域替换文字`}
                          value={region.replacement ?? ""}
                          disabled={!reviewing}
                          onChange={(event) =>
                            editRegion({
                              ...region,
                              replacement: event.target.value,
                            })
                          }
                        />
                      </span>
                      {pendingRegions.has(region.id) && (
                        <LoaderCircle
                          size={15}
                          className="spin"
                          aria-label="正在保存区域"
                        />
                      )}
                      <button
                        className="icon-button danger"
                        aria-label="删除区域"
                        title="删除区域"
                        disabled={!reviewing}
                        onClick={() => removeRegion(region)}
                      >
                        <Trash2 size={16} />
                      </button>
                    </div>
                  ))}
                  {!manualRegions.length && (
                    <p className="empty">
                      还没有手工区域，可直接在页面上拖动框选。
                    </p>
                  )}
                </div>
              </div>
            )}
          </aside>
        )}
      </div>

      {deletedRegion && (
        <div className="undo-toast" role="status">
          <span>已删除第 {deletedRegion.page + 1} 页的区域</span>
          <button className="secondary" onClick={restoreDeletedRegion}>
            <RotateCcw size={15} />
            撤销删除
          </button>
        </div>
      )}

      {task.meta.state === "completed" && (
        <section className="export-options">
          <details className="recovery-options">
            <summary>
              <KeyRound size={16} />
              创建可逆恢复包
            </summary>
            <p className="hint-banner">
              口令无法找回。恢复包包含可还原的原始内容，请与脱敏文件分开保管。
            </p>
            <div className="password-grid">
              <label>
                恢复口令
                <span className="password-field">
                  <input
                    type={showPassword ? "text" : "password"}
                    aria-label="恢复口令"
                    placeholder="至少 8 个字符"
                    value={password}
                    onChange={(event) => setPassword(event.target.value)}
                  />
                  <button
                    type="button"
                    className="icon-button secondary"
                    aria-label={showPassword ? "隐藏口令" : "显示口令"}
                    onClick={() => setShowPassword(!showPassword)}
                  >
                    {showPassword ? <EyeOff size={17} /> : <Eye size={17} />}
                  </button>
                </span>
              </label>
              <label>
                再次输入
                <input
                  type={showPassword ? "text" : "password"}
                  aria-label="确认恢复口令"
                  value={confirmPassword}
                  onChange={(event) => setConfirmPassword(event.target.value)}
                />
              </label>
            </div>
            {confirmPassword && password !== confirmPassword && (
              <p className="field-error" role="alert">
                两次输入的口令不一致
              </p>
            )}
            <button
              className="secondary"
              disabled={
                working ||
                [...password].length < 8 ||
                password !== confirmPassword
              }
              onClick={() =>
                action.run(async () => {
                  const path = await save({ defaultPath: "恢复包.ldsrec" });
                  if (path) {
                    await call("export_recovery", {
                      id: task.meta.id,
                      path,
                      password,
                    });
                    setPassword("");
                    setConfirmPassword("");
                    action.setNotice("已保存加密恢复包，请妥善保管口令");
                  }
                })
              }
            >
              保存可逆恢复包
            </button>
          </details>
        </section>
      )}
    </div>
  );
}
function Rules() {
  const rules = useQuery({
    queryKey: ["rules"],
    queryFn: () => call("list_rules"),
  });
  const action = useAction();
  const [name, setName] = useState("");
  const [type, setType] = useState("");
  const [kind, setKind] = useState<Rule["kind"]>("literal");
  const [pattern, setPattern] = useState("");
  const [editing, setEditing] = useState<Rule | null>(null);
  const [sample, setSample] = useState("");
  const [matches, setMatches] = useState<Entity[] | null>(null);
  return (
    <>
      <Heading title="识别规则">
        定义需要识别的内容。创建自定义类型时，会自动添加对应脱敏方式。
      </Heading>
      <Feedback
        {...action}
        error={action.error || (rules.error ? message(rules.error) : "")}
      />
      <section className="card">
        <h2>{editing ? "编辑规则" : "添加规则"}</h2>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            action.run(async () => {
              await call("save_rule", {
                rule: {
                  id: editing?.id ?? crypto.randomUUID(),
                  name,
                  entity_type: type,
                  kind,
                  pattern,
                  enabled: editing?.enabled ?? true,
                },
              });
              setName("");
              setPattern("");
              setEditing(null);
              setMatches(null);
              await rules.refetch();
            });
          }}
        >
          <div className="toolbar">
            <input
              required
              aria-label="规则名称"
              placeholder="规则名称"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            <input
              required
              aria-label="实体类型"
              placeholder="实体类型，例如：内部项目"
              value={type}
              onChange={(e) => setType(e.target.value)}
            />
            <select
              aria-label="规则种类"
              value={kind}
              onChange={(e) => setKind(e.target.value as Rule["kind"])}
            >
              <option value="literal">固定文字</option>
              <option value="regex">安全正则</option>
              <option value="dictionary">词典（每行一项）</option>
            </select>
          </div>
          <textarea
            required
            aria-label="规则内容"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder="输入需要识别的内容"
          />
          <button disabled={action.busy}>保存规则</button>
          {editing && (
            <button
              type="button"
              className="secondary"
              onClick={() => {
                setEditing(null);
                setName("");
                setType("");
                setPattern("");
                setMatches(null);
              }}
            >
              取消编辑
            </button>
          )}
        </form>
        <details className="rule-test">
          <summary>试一下识别效果</summary>
          <textarea
            aria-label="规则测试文本"
            placeholder="输入一小段样例，不会保存为任务"
            value={sample}
            onChange={(event) => setSample(event.target.value)}
          />
          <button
            className="secondary"
            disabled={
              action.busy || !pattern.trim() || !sample.trim() || !type.trim()
            }
            onClick={() =>
              action.run(async () => {
                setMatches(
                  await call("test_rule", {
                    rule: {
                      id: editing?.id ?? crypto.randomUUID(),
                      name: name || "试用规则",
                      entity_type: type,
                      kind,
                      pattern,
                      enabled: true,
                    },
                    text: sample,
                  }),
                );
              })
            }
          >
            测试规则
          </button>
          {matches && (
            <p role="status">
              匹配 {matches.length} 处
              {matches.length
                ? `：${matches.map((item) => item.text).join("、")}`
                : "，请检查规则内容。"}
            </p>
          )}
        </details>
      </section>
      <section className="card">
        <h2>已保存规则</h2>
        {rules.data?.map((r) => (
          <div className="rule" key={r.id}>
            <label>
              <input
                type="checkbox"
                checked={r.enabled}
                disabled={action.busy}
                onChange={() =>
                  action.run(async () => {
                    await call("save_rule", {
                      rule: { ...r, enabled: !r.enabled },
                    });
                    await rules.refetch();
                  })
                }
              />
              {r.name} <span className="tag">{r.entity_type}</span>
            </label>
            <button
              className="secondary"
              disabled={action.busy}
              onClick={() => {
                setEditing(r);
                setName(r.name);
                setType(r.entity_type);
                setKind(r.kind);
                setPattern(r.pattern);
                setMatches(null);
                window.scrollTo({ top: 0, behavior: "smooth" });
              }}
            >
              编辑
            </button>
            <button
              className="danger"
              disabled={action.busy}
              onClick={() =>
                action.run(async () => {
                  if (!window.confirm(`确定删除规则“${r.name}”吗？`)) return;
                  await call("delete_rule", { id: r.id });
                  await rules.refetch();
                })
              }
            >
              删除
            </button>
          </div>
        ))}
      </section>
    </>
  );
}
function Policies() {
  const policies = useQuery({
    queryKey: ["policies"],
    queryFn: () => call("list_policies"),
  });
  const builtins = useQuery({
    queryKey: ["builtin-policies"],
    queryFn: () => call("builtin_policies"),
  });
  const action = useAction();
  const [entityType, setType] = useState("");
  const [replacement, setReplacement] = useState("");
  return (
    <>
      <Heading title="脱敏方式">
        设置识别到内容后如何替换；保存的方式应用于新建任务。
      </Heading>
      <Feedback
        {...action}
        error={
          action.error ||
          (policies.error ? message(policies.error) : "") ||
          (builtins.error ? message(builtins.error) : "")
        }
      />
      <details className="builtin-policies">
        <summary>
          系统内置方式 <span>{builtins.data?.length ?? 0} 种</span>
        </summary>
        <div className="builtin-policy-list">
          {builtins.data?.map((item) => {
            const override = policies.data?.find(
              (policy) => policy.entity_type === item.entity_type,
            );
            return (
              <div className="builtin-policy" key={item.entity_type}>
                <div className="builtin-policy-name">
                  <strong>{item.type_label}</strong>
                  <span className="tag">{item.entity_type}</span>
                  {override && <span className="badge">已自定义</span>}
                </div>
                <div>
                  {override
                    ? `当前替换为：${override.replacement || "（删除）"}`
                    : item.behavior}
                  {!override &&
                    item.examples.map((example) => (
                      <small key={example.original}>
                        {example.original} → {example.replacement}
                      </small>
                    ))}
                </div>
                <button
                  type="button"
                  className="secondary"
                  onClick={() => {
                    setType(item.entity_type);
                    setReplacement(override?.replacement ?? "");
                    document
                      .getElementById("policy-editor")
                      ?.scrollIntoView({ block: "center" });
                  }}
                >
                  {override ? "编辑" : "自定义"}
                </button>
              </div>
            );
          })}
        </div>
      </details>
      <section className="card">
        <form
          id="policy-editor"
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault();
            action.run(async () => {
              await call("save_policy", {
                policy: { entity_type: entityType, replacement },
              });
              await policies.refetch();
            });
          }}
        >
          <input
            required
            aria-label="实体类型"
            placeholder="类型代码或自定义类型"
            value={entityType}
            onChange={(e) => setType(e.target.value)}
          />
          <input
            aria-label="替换文字"
            placeholder="替换文字（空白表示删除）"
            value={replacement}
            onChange={(e) => setReplacement(e.target.value)}
          />
          <button disabled={action.busy}>保存脱敏方式</button>
        </form>
        <table>
          <thead>
            <tr>
              <th>类型</th>
              <th>替换文字</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {policies.data?.map((p) => (
              <tr key={p.entity_type}>
                <td>{p.entity_type}</td>
                <td>{p.replacement || "（删除）"}</td>
                <td>
                  <button
                    className="secondary"
                    onClick={() => {
                      setType(p.entity_type);
                      setReplacement(p.replacement);
                    }}
                  >
                    编辑
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </>
  );
}
function Models() {
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  const packages = useQuery({
    queryKey: ["model-packages"],
    queryFn: () => call("model_packages"),
  });
  const queryClient = useQueryClient();
  const action = useAction();
  const [progress, setProgress] = useState<
    Record<string, ModelProgressPayload>
  >({});
  useEffect(() => {
    const unlisten = listen<ModelProgressPayload>(
      "model-progress",
      ({ payload }) => {
        if (!payload.id) return;
        if (["ready", "failed", "cancelled"].includes(payload.stage ?? "")) {
          void queryClient.invalidateQueries({ queryKey: ["model-packages"] });
          void queryClient.invalidateQueries({ queryKey: ["model"] });
          setProgress((current) => {
            const next = { ...current };
            delete next[payload.id!];
            return next;
          });
          return;
        }
        setProgress((current) => ({
          ...current,
          [payload.id!]: {
            id: payload.id,
            stage: payload.stage,
            current: payload.current,
            total: payload.total,
            percent: payload.percent ?? 0,
            bytes_per_second: payload.bytes_per_second,
            eta_seconds: payload.eta_seconds,
            source: payload.source,
            source_label: payload.source_label,
            message: payload.message ?? "正在处理模型",
          },
        }));
      },
    );
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);
  return (
    <>
      <Heading title="模型管理">
        模型在本机执行，优先从阿里云 OSS 下载；连接失败时自动切换到 ModelScope
        备用源。
      </Heading>
      <Feedback
        {...action}
        error={
          action.error ||
          model.data?.error ||
          (model.error ? message(model.error) : "") ||
          (packages.error ? message(packages.error) : "")
        }
      />
      <section
        className={`model-summary ${model.data?.ready ? "ready" : ""}`}
        aria-live="polite"
      >
        <div>
          <strong>
            {model.data?.ready ? "基础识别已就绪" : "需要完成基础模型准备"}
          </strong>
          <span>
            {model.data?.ready
              ? "现在可以处理文本；图片和扫描件取决于下方 OCR 状态。"
              : "至少需要安装并加载中文实体识别模型。"}
          </span>
        </div>
        <button
          className="secondary"
          disabled={action.busy}
          onClick={() =>
            action.run(async () => {
              const result = await call("load_models");
              queryClient.setQueryData(["model"], result);
              await packages.refetch();
            })
          }
        >
          完整校验并加载
        </button>
      </section>
      <div className="model-grid">
        {packages.data?.map((item) => (
          <section className="card" key={item.id}>
            <div className="toolbar">
              <h2>
                {item.profile === "text"
                  ? "中文实体识别 · RaNER"
                  : `图片识别 · PP-OCRv4 ${item.profile === "mobile" ? "轻量" : "高精度"}`}
              </h2>
              <span className={`badge ${item.ready ? "" : "warning-badge"}`}>
                {
                  (
                    {
                      missing: "尚未安装",
                      downloading: "正在下载",
                      installed: "已安装，尚未加载",
                      loading: "正在加载",
                      ready: "已安装并可用",
                      failed: "加载失败",
                    } as Record<string, string>
                  )[
                    item.state ??
                      (item.ready
                        ? "ready"
                        : item.error
                          ? "failed"
                          : item.installed
                            ? "installed"
                            : "missing")
                  ]
                }
              </span>
            </div>
            <dl>
              <dt>版本</dt>
              <dd>{item.version}</dd>
              <dt>下载大小</dt>
              <dd>{(item.size / 1024 / 1024).toFixed(1)} MB</dd>
              <dt>磁盘位置</dt>
              <dd>{item.location}</dd>
            </dl>
            {item.error && <p className="error">{item.error}</p>}
            {progress[item.id] && (
              <div className="download-progress">
                <progress max="100" value={progress[item.id].percent} />
                <div className="download-progress-head">
                  <span>{progress[item.id].message}</span>
                  <strong>
                    {(progress[item.id].percent ?? 0).toFixed(0)}%
                  </strong>
                </div>
                {progress[item.id].source_label && (
                  <span className="download-source">
                    当前来源：{progress[item.id].source_label}
                  </span>
                )}
                {modelProgressDetails(progress[item.id]) && (
                  <small>{modelProgressDetails(progress[item.id])}</small>
                )}
              </div>
            )}
            <div className="toolbar">
              {item.installed && !item.ready && (
                <button
                  disabled={action.busy || item.state === "loading"}
                  onClick={() =>
                    action.run(async () => {
                      const result = await call("retry_model_load", {
                        packageId: item.id,
                      });
                      queryClient.setQueryData(["model"], result);
                      await packages.refetch();
                    })
                  }
                >
                  {item.state === "loading"
                    ? "正在加载…"
                    : item.error
                      ? "重试加载"
                      : "加载模型"}
                </button>
              )}
              <button
                className={item.installed ? "secondary" : undefined}
                disabled={action.busy}
                title={action.busy ? "已有模型操作正在进行" : undefined}
                onClick={() => {
                  if (
                    item.installed &&
                    !window.confirm(
                      `重建“${item.profile === "text" ? "中文实体识别" : item.profile === "mobile" ? "轻量 OCR" : "高精度 OCR"}”模型？这会先删除当前模型，再重新下载。下载失败期间该识别能力不可用。`,
                    )
                  )
                    return;
                  action.run(async () => {
                    const result = await call(
                      item.installed ? "rebuild_model" : "install_model",
                      {
                        packageId: item.id,
                      },
                    );
                    queryClient.setQueryData(["model"], result);
                    await packages.refetch();
                    setProgress((current) => {
                      const next = { ...current };
                      delete next[item.id];
                      return next;
                    });
                  });
                }}
              >
                {item.installed
                  ? "重建模型"
                  : item.profile === "accurate"
                    ? "按需下载"
                    : "下载并安装"}
              </button>
              {progress[item.id] &&
                canCancelModelProgress(progress[item.id].stage) && (
                  <button
                    className="secondary"
                    onClick={() =>
                      call("cancel_model_install", { packageId: item.id })
                    }
                  >
                    取消下载
                  </button>
                )}
            </div>
          </section>
        ))}
      </div>
    </>
  );
}
function Restore() {
  const action = useAction();
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  return (
    <>
      <Heading title="恢复文件">
        用生成恢复包时设置的口令，恢复原始文件。
      </Heading>
      <Feedback {...action} />
      <section className="card">
        <h2>打开加密恢复包</h2>
        <p>恢复输出应保存到新位置，避免覆盖已有文件。</p>
        <div className="password-field restore-password">
          <input
            type={showPassword ? "text" : "password"}
            aria-label="恢复口令"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="请输入恢复口令"
          />
          <button
            type="button"
            className="icon-button secondary"
            aria-label={showPassword ? "隐藏口令" : "显示口令"}
            title={showPassword ? "隐藏口令" : "显示口令"}
            onClick={() => setShowPassword(!showPassword)}
          >
            {showPassword ? <EyeOff size={17} /> : <Eye size={17} />}
          </button>
        </div>
        <button
          disabled={!password || action.busy}
          onClick={() =>
            action.run(async () => {
              const source = await open({
                multiple: false,
                filters: [{ name: "加密恢复包", extensions: ["ldsrec"] }],
              });
              if (typeof source !== "string") return;
              const destination = await save({ title: "保存恢复后的原始文件" });
              if (!destination) return;
              await call("restore", { source, destination, password });
              setPassword("");
              action.setNotice("已验证并恢复原始文件");
            })
          }
        >
          {action.busy ? "正在验证并恢复…" : "选择恢复包并保存原文件"}
        </button>
      </section>
    </>
  );
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={client}>
      <Lifecycle />
      <AuthGate />
    </QueryClientProvider>
  </React.StrictMode>,
);
