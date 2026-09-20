import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { open, save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  FileText,
  Square,
} from "lucide-react";
import { call, formatBytes, message, stateLabels } from "./api";
import { useWorkbench } from "./store";

export function Batch() {
  const [params, setParams] = useSearchParams();
  const id = params.get("id");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState("");
  const [notice, setNotice] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [onlyPending, setOnlyPending] = useState(false);
  const navigate = useNavigate();
  const task = useQuery({
    queryKey: ["batch", id],
    queryFn: () => call("batch_view", { id: id! }),
    enabled: !!id,
  });
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  const items = task.data?.items ?? [];
  const reviewable = items.filter((item) => !!item.task_id);
  const reviewed = reviewable.filter(
    (item) => (item.reviewed_revision ?? 0) > 0,
  );
  const visibleItems = onlyPending
    ? items.filter((item) => !!item.task_id && !(item.reviewed_revision ?? 0))
    : items;
  async function run(fn: () => Promise<void>) {
    setBusy(true);
    setError("");
    setNotice("");
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await listen<{ done: number; total: number }>(
        "batch-progress",
        (event) =>
          setProgress(`${event.payload.done} / ${event.payload.total}`),
      );
      await fn();
    } catch (error) {
      setError(message(error));
    } finally {
      unlisten?.();
      setBusy(false);
      setProgress("");
      setPendingId(null);
    }
  }
  return (
    <>
      <header>
        <h1>批量处理</h1>
        <p>选择文件，逐项复核，再统一生成和导出。失败项会保留在报告中。</p>
      </header>
      {(error || task.error) && (
        <p role="alert" className="error">
          {error || message(task.error)}
        </p>
      )}
      {notice && (
        <p role="status" className="notice">
          {notice}
        </p>
      )}
      <section className="card">
        <div className="toolbar">
          <button
            disabled={busy || !model.data?.ready}
            onClick={() =>
              run(async () => {
                const paths = await open({
                  multiple: true,
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
                        "docx",
                        "xlsx",
                        "xlsm",
                        "pdf",
                      ],
                    },
                  ],
                });
                if (!Array.isArray(paths) || !paths.length) return;
                const requestId = crypto.randomUUID();
                setPendingId(requestId);
                const batch = await call("create_batch", { paths, requestId });
                setParams({ id: batch.meta.id });
              })
            }
          >
            选择多个文件
          </button>
          <span>最多 20 个文件，总大小不超过 500 MB</span>
          {busy && <span role="status">正在处理 {progress}</span>}
          {busy && pendingId && (
            <button
              className="secondary"
              title="取消当前批次"
              onClick={() => void call("cancel_task", { id: pendingId })}
            >
              <Square size={16} />
              取消批次
            </button>
          )}
        </div>
        {!id && (
          <p className="empty">批次会保存在任务历史中，可以稍后继续复核。</p>
        )}
        {task.data && (
          <>
            <div className="batch-summary" aria-live="polite">
              <div>
                <strong>{items.length}</strong>
                <span>文件总数</span>
              </div>
              <div>
                <strong>{reviewed.length}</strong>
                <span>已复核</span>
              </div>
              <div>
                <strong>{reviewable.length - reviewed.length}</strong>
                <span>待复核</span>
              </div>
              <div>
                <strong>
                  {items.filter((item) => item.state === "failed").length}
                </strong>
                <span>失败</span>
              </div>
            </div>
            <div className="toolbar">
              <span className="badge">{stateLabels[task.data.meta.state]}</span>
              <button
                disabled={busy || task.data.meta.state !== "awaiting_review"}
                onClick={() =>
                  run(async () => {
                    const pending = reviewable.length - reviewed.length;
                    if (
                      pending > 0 &&
                      !window.confirm(
                        `还有 ${pending} 个文件未手工复核，将使用自动识别结果继续生成。确定继续吗？`,
                      )
                    )
                      return;
                    setPendingId(id!);
                    await call("execute_batch", { id: id! });
                    await task.refetch();
                  })
                }
              >
                按已保存的复核结果生成
              </button>
              <button
                disabled={
                  busy ||
                  !["completed", "partial"].includes(task.data.meta.state)
                }
                onClick={() =>
                  run(async () => {
                    const path = await save({
                      defaultPath: "批量脱敏结果.zip",
                    });
                    if (path) {
                      await call("export_batch", { id: id!, path });
                      setNotice("已导出 ZIP 和报告");
                    }
                  })
                }
              >
                导出 ZIP 和报告
              </button>
              <label className="filter-toggle">
                <input
                  type="checkbox"
                  checked={onlyPending}
                  onChange={(event) => setOnlyPending(event.target.checked)}
                />
                只看未复核
              </label>
            </div>
            <p>每项复核结果会自动保存。生成前会再次提醒尚未复核的文件。</p>
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>文件</th>
                    <th>状态</th>
                    <th>说明</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleItems.map((item, visibleIndex) => (
                    <tr key={item.index}>
                      <td>
                        <span className="file-cell">
                          <FileText size={17} />
                          <span>
                            <strong>
                              {item.display_name ||
                                `第 ${item.index + 1} 个文件`}
                            </strong>
                            <small>
                              {(item.extension || "文件").toUpperCase()} ·{" "}
                              {formatBytes(item.file_size)}
                            </small>
                          </span>
                        </span>
                      </td>
                      <td>
                        {stateLabels[item.state]}
                        {(item.reviewed_revision ?? 0) > 0 && (
                          <span className="reviewed">
                            <CheckCircle2 size={14} />
                            已复核
                          </span>
                        )}
                      </td>
                      <td>
                        {item.error_info ? (
                          <>
                            <strong>{item.error_info.title}</strong>
                            <small>{item.error_info.recovery_action}</small>
                          </>
                        ) : (
                          item.error || "—"
                        )}
                      </td>
                      <td>
                        <div className="row-actions">
                          <button
                            className="icon-button secondary"
                            aria-label="上一个文件"
                            title="上一个文件"
                            disabled={visibleIndex === 0}
                            onClick={() =>
                              document
                                .getElementById(
                                  `batch-${visibleItems[visibleIndex - 1]?.index}`,
                                )
                                ?.scrollIntoView({
                                  behavior: "smooth",
                                  block: "center",
                                })
                            }
                          >
                            <ArrowLeft size={16} />
                          </button>
                          <button
                            className="secondary"
                            id={`batch-${item.index}`}
                            disabled={
                              busy ||
                              !item.task_id ||
                              !["awaiting_review", "completed"].includes(
                                item.state,
                              )
                            }
                            onClick={() =>
                              run(async () => {
                                const store = useWorkbench.getState();
                                store.setTask(
                                  await call("task_view", {
                                    id: item.task_id!,
                                  }),
                                );
                                store.setBatchId(id);
                                navigate("/");
                              })
                            }
                          >
                            {(item.reviewed_revision ?? 0) > 0
                              ? "再次复核"
                              : "打开复核"}
                          </button>
                          <button
                            className="icon-button secondary"
                            aria-label="下一个文件"
                            title="下一个文件"
                            disabled={visibleIndex === visibleItems.length - 1}
                            onClick={() =>
                              document
                                .getElementById(
                                  `batch-${visibleItems[visibleIndex + 1]?.index}`,
                                )
                                ?.scrollIntoView({
                                  behavior: "smooth",
                                  block: "center",
                                })
                            }
                          >
                            <ArrowRight size={16} />
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {visibleItems.length === 0 && (
              <p className="empty">所有可处理文件都已复核。</p>
            )}
          </>
        )}
      </section>
    </>
  );
}
