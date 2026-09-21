import React, { useEffect, useRef, useState } from "react";
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
  selection,
  stateLabels,
  type Entity,
  type DocumentPreview,
  type Region,
  regionUpdate,
  type Rule,
  type TaskOptions,
  capabilityReady,
  formatBytes,
  type ModelStatus,
  type TaskMeta,
  type AuthStatus,
} from "./api";
import { useWorkbench } from "./store";
import "./style.css";
import { Batch } from "./Batch";
import sixaMark from "./assets/sixa-mark.svg";

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
  return { busy, error, notice, setNotice, run };
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
          await call("auth_logout");
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
  const activeTaskId = useWorkbench((state) => state.task?.meta.id ?? null);
  const inActiveWorkbench = location.pathname === "/" && !!activeTaskId;
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const autoCollapsedTask = useRef<string | null>(null);
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
    if (inActiveWorkbench && activeTaskId !== autoCollapsedTask.current) {
      autoCollapsedTask.current = activeTaskId;
      setSidebarCollapsed(true);
    } else if (!inActiveWorkbench) {
      setSidebarCollapsed(false);
    }
  }, [activeTaskId, inActiveWorkbench]);
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
      }),
      listen<ModelProgressPayload>("model-progress", ({ payload }) => {
        if (payload && typeof payload.ready === "boolean")
          client.setQueryData(["model"], payload);
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
      .then(() => (active ? call("ensure_default_models") : null))
      .then((m) => {
        if (!active || !m) return;
        client.setQueryData(["model"], m);
        setPrepareError("");
      })
      .catch((error) => active && setPrepareError(message(error)))
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
        <nav>
          {[
            ["/", FileText, "脱敏工作台"],
            ["/batch", FolderOpen, "批量处理"],
            ["/history", History, "任务历史"],
            ["/rules", ScanSearch, "识别规则"],
            ["/policies", SlidersHorizontal, "脱敏方式"],
            ["/models", Database, "模型管理"],
            ["/restore", KeyRound, "恢复文件"],
            ["/integration", Cable, "AI 工具接入"],
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
            onClick={() => void onLogout()}
          >
            <LogOut size={16} />
          </button>
        </div>
        <button
          type="button"
          className="sidebar-toggle secondary"
          aria-label={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}
          title={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}
          onClick={() => setSidebarCollapsed((value) => !value)}
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
        {preparing ? (
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

function Highlight({ text, entities }: { text: string; entities: Entity[] }) {
  let cursor = 0;
  const parts: React.ReactNode[] = [];
  for (const e of [...entities].sort(
    (a, b) => a.display.start - b.display.start,
  )) {
    parts.push(text.slice(cursor, e.display.start));
    parts.push(
      <mark
        key={e.id}
        className={e.selected ? "" : "unselected"}
        title={e.type_label}
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
  const request = call("document_preview", { id });
  documentPreviewCache.set(id, request);
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
}: {
  preview: DocumentPreview;
  regions: Region[];
  result: boolean;
  editable: boolean;
  showHelpers: boolean;
  zoom: number;
  onCreate?: (region: Region) => void;
}) {
  const [draft, setDraft] = useState<{
    page: number;
    pointerId: number;
    start: { x: number; y: number };
    end: { x: number; y: number };
  } | null>(null);
  useEffect(() => {
    if (!draft) return;
    const cancel = (event: KeyboardEvent) => {
      if (event.key === "Escape") setDraft(null);
    };
    window.addEventListener("keydown", cancel);
    return () => window.removeEventListener("keydown", cancel);
  }, [draft]);
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
        const pageRegions = regions.filter(
          (region) => region.page === page.index,
        );
        const currentDraft = draft?.page === page.index ? draft : null;
        return (
          <div
            className="page-preview"
            key={page.index}
            style={{ width: `${zoom * 100}%`, maxWidth: `${900 * zoom}px` }}
          >
            <img src={page.preview_uri} alt={`第 ${page.index + 1} 页`} />
            {result &&
              pageRegions
                .filter((region) => region.selected)
                .map((region) => {
                  const xs = region.polygon.map((item) => item.x);
                  const ys = region.polygon.map((item) => item.y);
                  const left = Math.min(...xs);
                  const top = Math.min(...ys);
                  return (
                    <div
                      className="redaction-preview"
                      key={region.id}
                      data-region-id={region.id}
                      style={{
                        left: `${left * 100}%`,
                        top: `${top * 100}%`,
                        width: `${(Math.max(...xs) - left) * 100}%`,
                        height: `${(Math.max(...ys) - top) * 100}%`,
                      }}
                    >
                      <span>{region.replacement || "已脱敏"}</span>
                    </div>
                  );
                })}
            <svg
              viewBox="0 0 1 1"
              preserveAspectRatio="none"
              className={editable ? "region-layer editable" : "region-layer"}
              data-testid={`region-canvas-page-${page.index}`}
              onPointerDown={(event) => {
                if (!editable || event.button !== 0) return;
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
                if (!draft || draft.pointerId !== event.pointerId) return;
                setDraft({ ...draft, end: point(event) });
              }}
              onPointerUp={(event) => finish(event, page.index)}
              onPointerCancel={(event) => {
                if (draft?.pointerId === event.pointerId) setDraft(null);
              }}
            >
              {!result &&
                pageRegions
                  .filter(
                    (region) =>
                      showHelpers ||
                      region.source === "manual" ||
                      region.source === "entity",
                  )
                  .map((region) => (
                    <polygon
                      key={region.id}
                      data-region-id={region.id}
                      points={region.polygon
                        .map((item) => `${item.x},${item.y}`)
                        .join(" ")}
                      className={`${region.source} ${region.selected ? "selected" : ""}`}
                      vectorEffect="non-scaling-stroke"
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
            <span className="page-number">第 {page.index + 1} 页</span>
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
    setRevision,
    undo,
    undoLast,
    dirty,
    markSaved,
    batchId,
    setBatchId,
  } = useWorkbench();
  const navigate = useNavigate();
  const [filter, setFilter] = useState("");
  const [textPreview, setTextPreview] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [autoSaving, setAutoSaving] = useState(false);
  const [autoSaveError, setAutoSaveError] = useState("");
  const [documentPreview, setDocumentPreview] =
    useState<DocumentPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState("");
  const [resultDocumentPreview, setResultDocumentPreview] =
    useState<DocumentPreview | null>(null);
  const [resultLoading, setResultLoading] = useState(false);
  const [resultError, setResultError] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [previewMode, setPreviewMode] = useState<PreviewMode>("result");
  const [inspectorTab, setInspectorTab] = useState<InspectorTab>("entities");
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [showHelpers, setShowHelpers] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pendingRegions, setPendingRegions] = useState<Set<string>>(new Set());
  const [deletedRegion, setDeletedRegion] = useState<Region | null>(null);
  const [regionError, setRegionError] = useState("");
  const undoDeleteTimer = useRef<number | null>(null);
  const regionQueue = useRef<Promise<void>>(Promise.resolve());
  const regionQueueError = useRef<unknown>(null);
  const revisionRef = useRef(0);
  const sourcePaneRef = useRef<HTMLDivElement>(null);
  const resultPaneRef = useRef<HTMLDivElement>(null);
  const splitScrollLock = useRef(false);
  const [options, setOptions] = useState<TaskOptions>({
    ocr_profile: "mobile",
    pdf_mode: "safe_rebuild",
  });
  const action = useAction();
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  const reviewing = task?.meta.state === "awaiting_review";
  const mobileReady = capabilityReady(model.data, "ppocrv4-mobile-v1");
  const accurateReady = capabilityReady(model.data, "ppocrv4-accurate-v1");
  const taskId = task?.meta.id ?? null;
  const taskState = task?.meta.state;

  useEffect(() => {
    revisionRef.current = task?.revision ?? 0;
  }, [task?.revision, taskId]);

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
    call("document_result_preview", { id: taskId })
      .then((value) => active && setResultDocumentPreview(value))
      .catch((error) => active && setResultError(message(error)))
      .finally(() => active && setResultLoading(false));
    return () => {
      active = false;
    };
  }, [taskId, taskState]);

  useEffect(() => {
    if (!taskId || !documentPreview || documentPreview.pages.length > 0) return;
    let active = true;
    setResultLoading(true);
    call("preview", { id: taskId })
      .then((value) => active && setTextPreview(value))
      .catch((error) => active && setPreviewError(message(error)))
      .finally(() => active && setResultLoading(false));
    return () => {
      active = false;
    };
  }, [documentPreview, task?.revision, taskId]);

  useEffect(
    () => () => {
      if (undoDeleteTimer.current !== null)
        window.clearTimeout(undoDeleteTimer.current);
    },
    [],
  );

  const refreshTask = async (id: string) => {
    const current = await call("task_view", { id });
    if (useWorkbench.getState().task?.meta.id === id) {
      revisionRef.current = current.revision;
      setTask(current);
    }
  };

  const syncSplitScroll = (
    event: React.UIEvent<HTMLDivElement>,
    peer: React.RefObject<HTMLDivElement | null>,
  ) => {
    if (splitScrollLock.current || !peer.current) return;
    const source = event.currentTarget;
    const target = peer.current;
    const verticalRange = source.scrollHeight - source.clientHeight;
    const horizontalRange = source.scrollWidth - source.clientWidth;
    splitScrollLock.current = true;
    target.scrollTop =
      verticalRange > 0
        ? (source.scrollTop / verticalRange) *
          Math.max(0, target.scrollHeight - target.clientHeight)
        : 0;
    target.scrollLeft =
      horizontalRange > 0
        ? (source.scrollLeft / horizontalRange) *
          Math.max(0, target.scrollWidth - target.clientWidth)
        : 0;
    window.requestAnimationFrame(() => {
      splitScrollLock.current = false;
    });
  };

  const queueRegionMutation = (
    regionId: string,
    mutation: (expectedRevision: number) => Promise<{ revision: number }>,
  ) => {
    setPendingRegions((current) => new Set(current).add(regionId));
    regionQueue.current = regionQueue.current
      .catch(() => undefined)
      .then(async () => {
        const ack = await mutation(revisionRef.current);
        revisionRef.current = ack.revision;
        setRevision(ack.revision);
        regionQueueError.current = null;
      })
      .catch(async (error) => {
        regionQueueError.current = error;
        setRegionError(message(error));
        if (taskId) await refreshTask(taskId);
        throw error;
      })
      .finally(() => {
        setPendingRegions((current) => {
          const next = new Set(current);
          next.delete(regionId);
          return next;
        });
      });
  };

  const waitForRegionQueue = async () => {
    await regionQueue.current;
    if (regionQueueError.current) throw regionQueueError.current;
  };

  const addRegion = (region: Region) => {
    if (!task) return;
    setRegionError("");
    replaceRegions([
      ...task.regions.filter((item) => item.id !== region.id),
      region,
    ]);
    setInspectorTab("regions");
    setInspectorOpen(true);
    queueRegionMutation(region.id, (expectedRevision) =>
      call(
        "upsert_region",
        regionUpdate(task.meta.id, region, expectedRevision),
      ),
    );
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
    queueRegionMutation(region.id, (expectedRevision) =>
      call("remove_region", {
        id: task.meta.id,
        regionId: region.id,
        expectedRevision,
      }),
    );
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
    const id = task.meta.id;
    const selections = selection(task.entities);
    const timer = window.setTimeout(async () => {
      setAutoSaving(true);
      setAutoSaveError("");
      try {
        await waitForRegionQueue();
        const updated = await call("select_entities", { id, selections });
        if (useWorkbench.getState().task?.meta.id === id) markSaved(updated);
      } catch (error) {
        setAutoSaveError(message(error));
      } finally {
        setAutoSaving(false);
      }
    }, 650);
    return () => window.clearTimeout(timer);
  }, [dirty, markSaved, reviewing, task?.entities, taskId]);

  const sync = async () => {
    const current = useWorkbench.getState().task;
    if (!current) throw Error("没有当前任务");
    await waitForRegionQueue();
    const updated = await call("select_entities", {
      id: current.meta.id,
      selections: selection(current.entities),
    });
    markSaved(updated);
    return updated;
  };

  const choose = () =>
    action.run(async () => {
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
          `${options.ocr_profile === "accurate" ? "高精度" : "轻量"} OCR 尚未就绪，请先前往模型管理安装并校验。`,
        );
      const requestId = crypto.randomUUID();
      setPendingId(requestId);
      try {
        setTask(await call("analyze_file", { path, options, requestId }));
      } finally {
        setPendingId(null);
      }
    });

  const exportFile = () =>
    action.run(async () => {
      if (!task) return;
      const path = await save({ defaultPath: `脱敏结果.${task.extension}` });
      if (path) {
        await call("export_task", { id: task.meta.id, path });
        action.setNotice("已导出脱敏文件");
      }
    });

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
              disabled={action.busy || !model.data?.ready}
            >
              {action.busy ? (
                <LoaderCircle size={17} className="spin" />
              ) : (
                <FolderOpen size={17} />
              )}
              {action.busy ? "正在识别文件…" : "选择本机文件"}
            </button>
            {action.busy && pendingId && (
              <button
                className="secondary"
                onClick={() => void call("cancel_task", { id: pendingId })}
              >
                <Square size={16} />
                取消任务
              </button>
            )}
          </div>
          {!model.data?.ready && (
            <p className="hint">
              中文实体识别模型尚未就绪。
              <NavLink to="/models">前往模型管理</NavLink>
            </p>
          )}
        </section>
      </>
    );
  }

  const liveRegions = task.regions.map((region) => {
    const entity = region.entity_id
      ? task.entities.find((item) => item.id === region.entity_id)
      : null;
    return entity
      ? {
          ...region,
          selected: entity.selected,
          replacement: entity.replacement ?? region.replacement,
        }
      : region;
  });
  const selectedCount = task.entities.filter(
    (entity) => entity.selected,
  ).length;
  const manualRegions = task.regions.filter(
    (region) => region.source === "manual",
  );
  const hasVisualPreview = !!documentPreview?.pages.length;

  const visual = (result: boolean, split = false) => {
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
                .then(setDocumentPreview)
                .catch((error) => setPreviewError(message(error)))
                .finally(() => setPreviewLoading(false));
            }}
          >
            重试
          </button>
        </div>
      );
    if (hasVisualPreview && documentPreview) {
      const completedResult =
        result && resultDocumentPreview?.pages.length
          ? resultDocumentPreview
          : null;
      return (
        <VisualPreview
          preview={completedResult ?? documentPreview}
          regions={completedResult ? [] : liveRegions}
          result={result && !completedResult}
          editable={reviewing && !action.busy && !split}
          showHelpers={showHelpers}
          zoom={zoom}
          onCreate={addRegion}
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
        <pre className="text-document">{textPreview}</pre>
      )
    ) : (
      <Highlight text={task.text} entities={task.entities} />
    );
  };

  return (
    <div className="task-content compact-workbench">
      <Feedback {...action} error={action.error || regionError} />
      <div className="task-commandbar">
        <div className="task-identity">
          {batchId && (
            <button
              className="icon-button secondary"
              aria-label="返回批次"
              title="返回批次"
              onClick={() => navigate(`/batch?id=${batchId}`)}
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
            disabled={!undo.length || action.busy}
            onClick={undoLast}
            title="撤销实体修改"
          >
            <Undo2 size={16} />
            撤销
          </button>
          <button
            className="secondary"
            disabled={action.busy}
            onClick={() => {
              if (
                dirty &&
                !window.confirm("当前修改尚未保存，确定新建任务吗？")
              )
                return;
              setTask(null);
              setBatchId(null);
              setPassword("");
              setConfirmPassword("");
            }}
          >
            新建任务
          </button>
          {reviewing && (
            <button
              disabled={action.busy}
              onClick={() =>
                action.run(async () => {
                  const id = task.meta.id;
                  await sync();
                  setPendingId(id);
                  try {
                    setTask(await call("execute", { id }));
                  } finally {
                    setPendingId(null);
                  }
                })
              }
            >
              {action.busy ? <LoaderCircle size={16} className="spin" /> : null}
              生成脱敏文件
            </button>
          )}
          {task.meta.state === "completed" && (
            <button disabled={action.busy} onClick={exportFile}>
              <Download size={16} />
              保存文件
            </button>
          )}
          {action.busy && pendingId && (
            <button
              className="secondary"
              onClick={() => void call("cancel_task", { id: pendingId })}
            >
              <Square size={16} />
              取消
            </button>
          )}
        </div>
      </div>

      {task.warnings.map((warning) => (
        <p className="hint-banner" key={warning}>
          {warning}
        </p>
      ))}

      <div
        className={`workspace-shell ${inspectorOpen ? "" : "inspector-closed"}`}
      >
        <section className="document-workspace" aria-label="文档预览">
          <div className="canvas-toolbar">
            <div className="segmented" aria-label="预览方式">
              <button
                className={previewMode === "source" ? "active" : ""}
                aria-pressed={previewMode === "source"}
                onClick={() => setPreviewMode("source")}
              >
                原件
              </button>
              <button
                className={previewMode === "result" ? "active" : ""}
                aria-pressed={previewMode === "result"}
                onClick={() => setPreviewMode("result")}
              >
                脱敏预览
              </button>
              <button
                className={previewMode === "split" ? "active" : ""}
                aria-pressed={previewMode === "split"}
                onClick={() => setPreviewMode("split")}
              >
                <Columns2 size={15} />
                并排
              </button>
            </div>
            {hasVisualPreview && (
              <>
                <button
                  className={`icon-button secondary ${showHelpers ? "active" : ""}`}
                  aria-label="显示 OCR 辅助框"
                  aria-pressed={showHelpers}
                  title="显示 OCR 辅助框"
                  onClick={() => setShowHelpers((value) => !value)}
                >
                  <ScanLine size={17} />
                </button>
                <div className="zoom-controls">
                  <button
                    className="icon-button secondary"
                    aria-label="缩小"
                    title="缩小"
                    disabled={zoom <= 0.65}
                    onClick={() =>
                      setZoom((value) => Math.max(0.65, value - 0.15))
                    }
                  >
                    <ZoomOut size={17} />
                  </button>
                  <button
                    className="zoom-value secondary"
                    onClick={() => setZoom(1)}
                    title="适合宽度"
                  >
                    {Math.round(zoom * 100)}%
                  </button>
                  <button
                    className="icon-button secondary"
                    aria-label="放大"
                    title="放大"
                    disabled={zoom >= 1.6}
                    onClick={() =>
                      setZoom((value) => Math.min(1.6, value + 0.15))
                    }
                  >
                    <ZoomIn size={17} />
                  </button>
                </div>
              </>
            )}
            {!inspectorOpen && (
              <button
                className="secondary inspector-open"
                onClick={() => setInspectorOpen(true)}
              >
                <ChevronsLeft size={16} />
                打开检查器
              </button>
            )}
          </div>
          <div
            className={`canvas-scroll ${previewMode === "split" ? "split-preview" : ""}`}
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
                  className="preview-pane"
                  ref={resultPaneRef}
                  onScroll={(event) => syncSplitScroll(event, sourcePaneRef)}
                >
                  <h2>脱敏预览</h2>
                  {visual(true, true)}
                </div>
              </>
            ) : (
              visual(previewMode === "result")
            )}
          </div>
          {resultLoading &&
            hasVisualPreview &&
            task.meta.state === "completed" && (
              <div className="render-indicator" role="status">
                <LoaderCircle size={15} className="spin" />
                正在加载最终结果
              </div>
            )}
          {resultError && previewMode !== "source" && (
            <div className="render-indicator render-error" role="alert">
              <CircleAlert size={15} />
              最终结果预览失败，当前显示即时预览
            </div>
          )}
        </section>

        {inspectorOpen && (
          <aside className="review-inspector" aria-label="复核检查器">
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
                    disabled={!reviewing || action.busy}
                    onClick={() =>
                      change(
                        task.entities.map((entity) => ({
                          ...entity,
                          selected: true,
                        })),
                      )
                    }
                  >
                    全选
                  </button>
                  <button
                    className="secondary"
                    disabled={!reviewing || action.busy}
                    onClick={() =>
                      change(
                        task.entities.map((entity) => ({
                          ...entity,
                          selected: false,
                        })),
                      )
                    }
                  >
                    清空
                  </button>
                </div>
                <div className="entity-list">
                  {task.entities
                    .filter((entity) =>
                      (entity.text + entity.type_label).includes(filter),
                    )
                    .map((entity) => (
                      <label className="entity-item" key={entity.id}>
                        <input
                          type="checkbox"
                          aria-label={`选择${entity.text}`}
                          checked={entity.selected}
                          disabled={!reviewing || action.busy}
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
                        <span className="entity-copy">
                          <strong>{entity.text}</strong>
                          <span>
                            <span className="tag">{entity.type_label}</span>
                            {Math.round(entity.score * 100)}% · {entity.source}
                          </span>
                        </span>
                        <input
                          aria-label={`${entity.text}的替换文字`}
                          placeholder="默认替换"
                          value={entity.replacement ?? ""}
                          disabled={!reviewing || action.busy}
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
                      </label>
                    ))}
                  {!task.entities.length && (
                    <p className="empty">
                      未识别到实体，请根据实际内容检查识别规则。
                    </p>
                  )}
                </div>
              </div>
            ) : (
              <div className="inspector-body" role="tabpanel">
                <p className="region-tip">
                  <ScanLine size={16} />
                  在页面上拖动鼠标即可框选脱敏区域，按 Esc 取消。
                </p>
                <div className="region-list">
                  {manualRegions.map((region) => (
                    <div className="region-item" key={region.id}>
                      <span>
                        <strong>第 {region.page + 1} 页</strong>
                        <small>{region.replacement || "已脱敏"}</small>
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
                action.busy ||
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
function Tasks() {
  const tasks = useQuery({
    queryKey: ["tasks"],
    queryFn: () => call("list_tasks"),
  });
  const action = useAction();
  const navigate = useNavigate();
  const { setTask } = useWorkbench();
  return (
    <>
      <Heading title="任务历史">在本机继续复核，或导出已经完成的任务。</Heading>
      <Feedback
        {...action}
        error={action.error || (tasks.error ? message(tasks.error) : "")}
      />
      <section className="card">
        <table>
          <thead>
            <tr>
              <th>文件</th>
              <th>状态</th>
              <th>更新时间</th>
              <th>占用空间</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {tasks.data
              ?.filter((t) => !t.parent_batch_id)
              .map((t) => (
                <tr key={t.id}>
                  <td>
                    <strong>
                      {t.display_name ||
                        (t.kind === "batch" ? "批量任务" : `未命名.${t.kind}`)}
                    </strong>
                    <small>
                      {t.kind.toUpperCase()} · {formatBytes(t.file_size)}
                    </small>
                  </td>
                  <td>
                    {stateLabels[t.state]}
                    {t.error_info ? (
                      <small>
                        {t.error_info.title}：{t.error_info.recovery_action}
                      </small>
                    ) : (
                      t.error && <small>{t.error}</small>
                    )}
                  </td>
                  <td>
                    {new Date(t.updated_at * 1000).toLocaleString("zh-CN")}
                  </td>
                  <td>{formatBytes(t.storage_bytes)}</td>
                  <td>
                    <button
                      className="secondary"
                      disabled={
                        action.busy ||
                        !(
                          t.kind === "batch" ||
                          ["awaiting_review", "completed"].includes(t.state)
                        )
                      }
                      onClick={() =>
                        action.run(async () => {
                          if (t.kind === "batch") {
                            navigate("/batch?id=" + t.id);
                          } else {
                            setTask(await call("task_view", { id: t.id }));
                            navigate("/");
                          }
                        })
                      }
                    >
                      打开
                    </button>
                    <button
                      className="danger"
                      disabled={action.busy}
                      onClick={() =>
                        action.run(async () => {
                          const label = t.display_name || "这项任务";
                          if (
                            !window.confirm(
                              `确定删除“${label}”吗？任务记录和本机缓存将一并删除。`,
                            )
                          )
                            return;
                          await call("delete_task", { id: t.id });
                          if (useWorkbench.getState().task?.meta.id === t.id)
                            setTask(null);
                          await tasks.refetch();
                        })
                      }
                    >
                      删除
                    </button>
                  </td>
                </tr>
              ))}
          </tbody>
        </table>
        {tasks.data?.length === 0 && (
          <p className="empty">还没有任务，从工作台开始。</p>
        )}
      </section>
    </>
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
        <h2>添加规则</h2>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            action.run(async () => {
              await call("save_rule", {
                rule: {
                  id: crypto.randomUUID(),
                  name,
                  entity_type: type,
                  kind,
                  pattern,
                  enabled: true,
                },
              });
              setName("");
              setPattern("");
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
        </form>
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
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: () => call("get_settings"),
  });
  const queryClient = useQueryClient();
  const action = useAction();
  const [progress, setProgress] = useState<
    Record<string, ModelProgressPayload>
  >({});
  useEffect(() => {
    const unlisten = listen<ModelProgressPayload>("model-progress", ({ payload }) => {
      if (!payload.id) return;
      if (["failed", "cancelled"].includes(payload.stage ?? "")) {
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
    });
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);
  return (
    <>
      <Heading title="模型管理">
        模型在本机执行，优先从阿里云 OSS 下载；连接失败时自动切换到 ModelScope 备用源。
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
                {item.ready
                  ? "已安装并可用"
                  : item.installed
                    ? "加载失败，可重建"
                    : "尚未安装"}
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
                  <strong>{(progress[item.id].percent ?? 0).toFixed(0)}%</strong>
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
              <button
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
      <section className="card">
        <h2>任务调度</h2>
        <label className="setting-line">
          同时处理任务数
          <select
            value={settings.data?.concurrency ?? 2}
            onChange={(event) => {
              if (!settings.data) return;
              action.run(async () => {
                const saved = await call("save_settings", {
                  settings: {
                    ...settings.data,
                    concurrency: Number(event.target.value),
                  },
                });
                queryClient.setQueryData(["settings"], saved);
              });
            }}
          >
            {[1, 2, 3, 4].map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>
      </section>
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
      <AuthGate />
    </QueryClientProvider>
  </React.StrictMode>,
);
