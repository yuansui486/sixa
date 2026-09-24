import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { call, message, type AppSettings, type CloseBehavior } from "./api";
import { usePreferences, type SidebarMode } from "./preferences";
export function Settings() {
  const preferences = usePreferences();
  const client = useQueryClient();
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: () => call("get_settings"),
  });
  const desktop = useQuery({
    queryKey: ["desktop-preferences"],
    queryFn: () => call("get_desktop_preferences"),
  });
  const [desktopBusy, setDesktopBusy] = useState(false);
  const [desktopError, setDesktopError] = useState("");
  const updateCloseBehavior = async (closeBehavior: CloseBehavior) => {
    setDesktopBusy(true);
    setDesktopError("");
    try {
      client.setQueryData(
        ["desktop-preferences"],
        await call("set_desktop_preferences", { closeBehavior }),
      );
    } catch (error) {
      setDesktopError(message(error));
    } finally {
      setDesktopBusy(false);
    }
  };
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const update = async (patch: Partial<AppSettings>) => {
    if (!settings.data) return;
    setBusy(true);
    setError("");
    try {
      client.setQueryData(
        ["settings"],
        await call("save_settings", {
          settings: { ...settings.data, ...patch },
        }),
      );
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <header>
        <h1>应用设置</h1>
        <p>新任务使用以下默认方式，当前任务保留原设置。</p>
      </header>
      {(error || settings.error) && (
        <p className="error" role="alert">
          {error || message(settings.error)}
        </p>
      )}
      <section className="card settings-section">
        <h2>处理方式</h2>
        <fieldset disabled={busy || !settings.data}>
          <label className="setting-line">
            图片识别
            <select
              value={settings.data?.ocr_profile ?? "mobile"}
              onChange={(e) =>
                void update({
                  ocr_profile: e.target.value as AppSettings["ocr_profile"],
                })
              }
            >
              <option value="mobile">轻量 · 速度优先</option>
              <option value="accurate">高精度 · 需先安装对应模型</option>
            </select>
          </label>
          <label className="setting-line">
            PDF 生成
            <select
              value={settings.data?.pdf_mode ?? "safe_rebuild"}
              onChange={(e) =>
                void update({
                  pdf_mode: e.target.value as AppSettings["pdf_mode"],
                })
              }
            >
              <option value="safe_rebuild">安全重建（推荐）</option>
              <option value="fidelity">保真脱敏</option>
            </select>
          </label>
          <label className="setting-line">
            同时处理文件数
            <select
              value={settings.data?.concurrency ?? 1}
              onChange={(e) =>
                void update({ concurrency: Number(e.target.value) })
              }
            >
              {[1, 2, 3, 4].map((n) => (
                <option value={n} key={n}>
                  {n}
                  {n === 1 ? " · 普通办公电脑推荐" : ""}
                </option>
              ))}
            </select>
          </label>
          <p className="muted">
            8 GB 内存建议一次处理 1 个文件；增加并发会占用更多内存。
          </p>
        </fieldset>
      </section>
      <section className="card settings-section">
        <h2>工作区布局</h2>
        <label className="setting-line">
          侧边栏
          <select
            aria-label="侧边栏"
            value={preferences.sidebar}
            onChange={(e) =>
              usePreferences.setState({
                sidebar: e.target.value as SidebarMode,
              })
            }
          >
            <option value="auto">自动 · 复核时折叠</option>
            <option value="expanded">固定展开</option>
            <option value="collapsed">固定折叠</option>
          </select>
        </label>
        <label className="setting-line">
          复核面板宽度
          <input
            aria-label="复核面板宽度"
            type="range"
            min={280}
            max={440}
            step={10}
            value={preferences.inspectorWidth}
            onChange={(e) =>
              usePreferences.setState({
                inspectorWidth: Number(e.target.value),
              })
            }
          />
          <span>{preferences.inspectorWidth} px</span>
        </label>
        <button
          className="secondary"
          onClick={() =>
            usePreferences.setState({
              sidebar: "auto",
              inspectorWidth: 320,
              inspectorOpen: true,
            })
          }
        >
          恢复默认布局
        </button>
      </section>
      <section className="card settings-section">
        <h2>关闭窗口</h2>
        {(desktopError || desktop.error) && (
          <p role="alert" className="error">
            {desktopError || message(desktop.error)}
          </p>
        )}
        <label className="setting-line">
          点击关闭按钮时
          <select
            aria-label="关闭窗口时"
            disabled={desktopBusy || !desktop.data}
            value={desktop.data?.close_behavior ?? "ask"}
            onChange={(event) =>
              void updateCloseBehavior(event.target.value as CloseBehavior)
            }
          >
            <option value="ask">每次询问（默认）</option>
            <option value="tray" disabled={!desktop.data?.tray_available}>
              最小化到托盘
              {desktop.data && !desktop.data.tray_available
                ? " · 当前不可用"
                : ""}
            </option>
            <option value="exit">直接退出</option>
          </select>
        </label>
        <p className="muted">
          后台运行会保留当前任务。直接退出前会保存修改，并确认是否停止正在运行的任务和下载。
        </p>
        {desktop.data && !desktop.data.tray_available && (
          <p className="muted">
            当前系统无法使用托盘，关闭时仍可选择直接退出或取消。
          </p>
        )}
      </section>
      <p className="muted">
        快捷键：Ctrl / ⌘ S 保存 · Ctrl / ⌘ Z 撤销 · Ctrl / ⌘ F 筛选实体 · Esc
        取消框选
      </p>
    </>
  );
}
