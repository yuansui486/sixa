import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { save } from "@tauri-apps/plugin-dialog";
import {
  call,
  formatBytes,
  message,
  stateLabels,
  type TaskMeta,
  type TaskState,
} from "./api";
import { useWorkbench } from "./store";
import { flushReview } from "./review";
import { clearPreviewCache } from "./preview-cache";
import { exportName, rememberDirectory } from "./preferences";
export function Tasks() {
  const [search, setSearch] = useState("");
  const [settledSearch, setSettledSearch] = useState("");
  const [state, setState] = useState<TaskState | "">("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [offset, setOffset] = useState(0);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [savedPath, setSavedPath] = useState("");
  const navigate = useNavigate();
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSettledSearch(search.trim());
      setOffset(0);
    }, 250);
    return () => clearTimeout(timer);
  }, [search]);
  const tasks = useQuery({
    queryKey: ["tasks", settledSearch, state, from, to, offset],
    queryFn: () =>
      call("query_tasks", {
        query: {
          search: settledSearch || undefined,
          state: state || undefined,
          from: from
            ? Math.floor(new Date(`${from}T00:00:00`).getTime() / 1000)
            : undefined,
          to: to
            ? Math.floor(new Date(`${to}T23:59:59`).getTime() / 1000)
            : undefined,
          offset,
          limit: 30,
        },
      }),
  });
  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
    }
  };
  const openTask = async (item: TaskMeta) => {
    await flushReview();
    if (item.kind === "batch") navigate(`/batch?id=${item.id}`);
    else {
      const store = useWorkbench.getState();
      store.setTask(await call("task_view", { id: item.id }));
      store.setBatchId(item.parent_batch_id ?? null);
      navigate("/");
    }
  };
  return (
    <>
      <header>
        <h1>任务历史</h1>
        <p>继续检查文件，查看处理结果或重新尝试。</p>
      </header>
      {(error || tasks.error) && (
        <p className="error" role="alert">
          {error || message(tasks.error)}
        </p>
      )}
      {savedPath && (
        <p className="export-notice">
          文件已保存
          <button
            className="text-action"
            onClick={() =>
              void run(async () => {
                await call("reveal_file", { path: savedPath });
              })
            }
          >
            打开所在文件夹
          </button>
        </p>
      )}
      <section className="card">
        <div className="history-tools">
          <label className="history-date">
            从
            <input
              type="date"
              aria-label="历史开始日期"
              value={from}
              max={to || undefined}
              onChange={(event) => {
                setFrom(event.target.value);
                setOffset(0);
              }}
            />
          </label>
          <label className="history-date">
            至
            <input
              type="date"
              aria-label="历史结束日期"
              value={to}
              min={from || undefined}
              onChange={(event) => {
                setTo(event.target.value);
                setOffset(0);
              }}
            />
          </label>
          <input
            aria-label="搜索历史文件"
            placeholder="搜索文件名"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <select
            aria-label="筛选任务状态"
            value={state}
            onChange={(e) => {
              setState(e.target.value as TaskState | "");
              setOffset(0);
            }}
          >
            <option value="">全部状态</option>
            {Object.entries(stateLabels).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
          <span>{tasks.data?.total ?? 0} 项</span>
        </div>
        {tasks.isLoading ? (
          <p className="empty" role="status">
            正在读取任务…
          </p>
        ) : (
          <div className="table-scroll">
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
                {tasks.data?.items.map((item) => (
                  <tr key={item.id}>
                    <td>
                      <strong>{item.display_name || "未命名任务"}</strong>
                      <small>
                        {item.kind.toUpperCase()} ·{" "}
                        {formatBytes(item.file_size)}
                      </small>
                    </td>
                    <td>
                      {stateLabels[item.state]}
                      {item.error_info && (
                        <details className="task-error">
                          <summary>{item.error_info.title}</summary>
                          <p>{item.error_info.detail}</p>
                          <p>{item.error_info.recovery_action}</p>
                        </details>
                      )}
                    </td>
                    <td>
                      {new Date(item.updated_at * 1000).toLocaleString("zh-CN")}
                    </td>
                    <td>{formatBytes(item.storage_bytes)}</td>
                    <td>
                      <div className="row-actions">
                        <button
                          className="secondary"
                          disabled={
                            busy ||
                            (item.kind !== "batch" &&
                              !["awaiting_review", "completed"].includes(
                                item.state,
                              ))
                          }
                          title={
                            ["failed", "cancelled"].includes(item.state)
                              ? "可以重新分析此任务"
                              : undefined
                          }
                          onClick={() => void run(() => openTask(item))}
                        >
                          打开
                        </button>
                        {item.state === "completed" &&
                          item.kind !== "batch" && (
                            <button
                              className="secondary"
                              disabled={busy}
                              onClick={() =>
                                void run(async () => {
                                  const path = await save({
                                    defaultPath: exportName(
                                      item.display_name,
                                      item.kind,
                                    ),
                                  });
                                  if (path) {
                                    await call("export_task", {
                                      id: item.id,
                                      path,
                                    });
                                    rememberDirectory(path);
                                    setSavedPath(path);
                                  }
                                })
                              }
                            >
                              保存文件
                            </button>
                          )}
                        {["failed", "cancelled"].includes(item.state) &&
                          item.kind !== "batch" && (
                            <button
                              className="secondary"
                              disabled={busy}
                              onClick={() =>
                                void run(async () => {
                                  await flushReview();
                                  useWorkbench
                                    .getState()
                                    .setTask(
                                      await call("retry_task", { id: item.id }),
                                    );
                                  useWorkbench.getState().setBatchId(null);
                                  navigate("/");
                                })
                              }
                            >
                              重新分析
                            </button>
                          )}
                        <button
                          className="text-action danger"
                          disabled={
                            busy ||
                            ["queued", "analyzing", "processing"].includes(
                              item.state,
                            )
                          }
                          onClick={() =>
                            void run(async () => {
                              if (
                                !window.confirm(
                                  `删除“${item.display_name || "此任务"}”及本机缓存？已导出的文件会保留。`,
                                )
                              )
                                return;
                              await flushReview();
                              await call("delete_task", { id: item.id });
                              clearPreviewCache(item.id);
                              if (
                                useWorkbench.getState().task?.meta.id ===
                                item.id
                              )
                                useWorkbench.getState().setTask(null);
                              await tasks.refetch();
                            })
                          }
                        >
                          删除
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {!tasks.isLoading && !tasks.data?.items.length && (
          <p className="empty">
            {search || state
              ? "没有符合条件的任务。"
              : "暂无任务，先选择一个文件开始处理。"}
          </p>
        )}
        <div className="pagination">
          <button
            className="secondary"
            disabled={offset === 0 || busy}
            onClick={() => setOffset(Math.max(0, offset - 30))}
          >
            上一页
          </button>
          <span>第 {Math.floor(offset / 30) + 1} 页</span>
          <button
            className="secondary"
            disabled={offset + 30 >= (tasks.data?.total ?? 0) || busy}
            onClick={() => setOffset(offset + 30)}
          >
            下一页
          </button>
        </div>
      </section>
    </>
  );
}
