import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { open, save } from "@tauri-apps/plugin-dialog";
import { LoaderCircle, Square, FileText } from "lucide-react";
import {
  call,
  formatBytes,
  message,
  stateLabels,
  capabilityInstalled,
  type BatchView,
} from "./api";
import { useWorkbench } from "./store";
import { flushReview } from "./review";
import { useJobs, runningJob } from "./jobs";
import { rememberDirectory } from "./preferences";

export function Batch() {
  const [params, setParams] = useSearchParams();
  const id = params.get("id");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [savedPath, setSavedPath] = useState("");
  const [filter, setFilter] = useState("all");
  const navigate = useNavigate();
  const client = useQueryClient();
  const job = useJobs((s) => (id ? s.jobs[id] : undefined));
  const active = runningJob(job);
  const busy = pending || active;
  const task = useQuery({
    queryKey: ["batch", id],
    queryFn: () => call("batch_view", { id: id! }),
    enabled: !!id,
    refetchInterval: (query) =>
      active ||
      ["queued", "analyzing", "processing"].includes(
        query.state.data?.meta.state ?? "",
      )
        ? 1500
        : false,
  });
  const model = useQuery({
    queryKey: ["model"],
    queryFn: () => call("model_status"),
  });
  const items = task.data?.items ?? [];
  const checkable = items.filter(
    (item) =>
      item.task_id && ["awaiting_review", "completed"].includes(item.state),
  );
  const checked = (item: BatchView["items"][number]) =>
    item.review_confirmed ??
    ((item.reviewed_revision ?? 0) > 0 &&
      (item.revision === undefined ||
        item.reviewed_revision === item.revision));
  const unreviewed = checkable.filter(
    (item) => item.state === "awaiting_review" && !checked(item),
  );
  const completed = items.filter((item) => item.state === "completed").length;
  const failed = items.filter((item) => item.state === "failed").length;
  const visibleItems = items.filter(
    (item) =>
      filter === "all" ||
      (filter === "pending"
        ? unreviewed.includes(item)
        : item.state === filter),
  );
  async function run(fn: () => Promise<void>) {
    setPending(true);
    setError("");
    try {
      await fn();
    } catch (error) {
      setError(message(error));
    } finally {
      setPending(false);
      void client.invalidateQueries({ queryKey: ["batch"] });
      void client.invalidateQueries({ queryKey: ["tasks"] });
    }
  }
  async function operate(
    command: "execute_batch" | "retry_batch",
    failedOnly = false,
  ) {
    if (!id) return;
    const operationId = command === "retry_batch" ? crypto.randomUUID() : id;
    if (operationId !== id) setParams({ id: operationId });
    useJobs
      .getState()
      .update({
        id: operationId,
        stage: "queued",
        batch: true,
        terminal: false,
      });
    try {
      const result =
        command === "execute_batch"
          ? await call(command, { id })
          : await call(command, { id, failedOnly, requestId: operationId });
      client.setQueryData(["batch", result.meta.id], result);
      if (
        result.meta.id !== operationId &&
        window.location.hash === `#/batch?id=${operationId}`
      )
        setParams({ id: result.meta.id });
    } finally {
      useJobs.getState().update({ id: operationId, terminal: true });
    }
  }
  return (
    <>
      <header>
        <h1>批量处理</h1>
        <p>逐份检查后统一生成，已完成的文件可以随时保存。</p>
      </header>
      {(error || (task.error && !active)) && (
        <p role="alert" className="error">
          {error || message(task.error)}
        </p>
      )}
      {savedPath && (
        <p className="export-notice" role="status">
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
        <div className="toolbar">
          <button
            disabled={busy || !capabilityInstalled(model.data, "raner-v1")}
            title={
              !capabilityInstalled(model.data, "raner-v1")
                ? "中文识别模型正在准备，请查看模型管理"
                : undefined
            }
            onClick={() =>
              void run(async () => {
                await flushReview();
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
                if (paths.length > 20)
                  throw Error("每批最多 20 个文件，请减少选择后重试");
                const settings = await call("get_settings");
                const ocrId =
                  settings.ocr_profile === "accurate"
                    ? "ppocrv4-accurate-v1"
                    : "ppocrv4-mobile-v1";
                if (
                  paths.some((path) =>
                    /\.(png|jpe?g|bmp|tiff?|pdf)$/i.test(path),
                  ) &&
                  !model.data?.capabilities?.some(
                    (capability) =>
                      capability.id === ocrId && capability.installed,
                  )
                )
                  throw Error(
                    "本批包含图片或 PDF，尚未安装所选图片识别模型，请在模型管理中完成安装",
                  );
                const requestId = crypto.randomUUID();
                useJobs.getState().update({
                  id: requestId,
                  stage: "queued",
                  total: paths.length,
                  done: 0,
                  terminal: false,
                  batch: true,
                });
                setParams({ id: requestId });
                try {
                  const batch = await call("create_batch", {
                    paths,
                    requestId,
                  });
                  client.setQueryData(["batch", requestId], batch);
                } finally {
                  useJobs.getState().update({ id: requestId, terminal: true });
                }
              })
            }
          >
            选择多个文件
          </button>
          <span className="muted">最多 20 份 · 共 500 MB</span>
          {!capabilityInstalled(model.data, "raner-v1") && (
            <button className="text-action" onClick={() => navigate("/models")}>
              查看模型准备进度
            </button>
          )}
          {busy && (
            <span role="status" className="inline-progress">
              <LoaderCircle size={16} className="spin" />
              {job?.stage === "queued"
                ? "等待处理"
                : job?.stage === "processing"
                  ? "正在生成"
                  : "正在分析"}
              {typeof job?.done === "number" &&
              typeof job.total === "number" &&
              job.total > 0
                ? ` · ${job.done} / ${job.total} 份`
                : ""}
            </span>
          )}
          {(active ||
            ["queued", "analyzing", "processing"].includes(
              task.data?.meta.state ?? "",
            )) &&
            id && (
              <button
                className="secondary"
                onClick={() =>
                  void run(async () => {
                    await call("cancel_task", { id });
                  })
                }
              >
                <Square size={15} />
                取消剩余任务
              </button>
            )}
        </div>
        {!id && (
          <p className="empty">选择文件后开始识别。批次会保存在任务历史中。</p>
        )}
        {id && !task.data && active && (
          <p className="empty" role="status">
            正在建立文件清单，可以离开此页，任务会继续处理。
          </p>
        )}
        {task.data && (
          <>
            <div className="batch-overview">
              <strong>{stateLabels[task.data.meta.state]}</strong>
              <span>{items.length} 份文件</span>
              <span>待检查 {unreviewed.length}</span>
              <span>已完成 {completed}</span>
              {failed > 0 && <span>失败 {failed}</span>}
            </div>
            <div className="toolbar">
              {task.data.meta.state === "awaiting_review" && (
                <button
                  disabled={busy}
                  onClick={() =>
                    void run(async () => {
                      if (
                        unreviewed.length &&
                        !window.confirm(
                          `还有 ${unreviewed.length} 份文件未确认检查，将按当前识别结果生成。继续吗？`,
                        )
                      )
                        return;
                      await operate("execute_batch");
                    })
                  }
                >
                  生成脱敏文件
                </button>
              )}
              {["failed", "cancelled", "partial"].includes(
                task.data.meta.state,
              ) && (
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() => void run(() => operate("retry_batch", false))}
                >
                  继续未完成文件
                </button>
              )}
              {failed > 0 && (
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() => void run(() => operate("retry_batch", true))}
                >
                  重试失败项
                </button>
              )}
              <button
                className="secondary"
                disabled={busy || !completed}
                title={!completed ? "生成成功后可以导出" : undefined}
                onClick={() =>
                  void run(async () => {
                    const path = await save({
                      defaultPath: "批量脱敏结果.zip",
                    });
                    if (path) {
                      await call("export_batch", { id: id!, path });
                      rememberDirectory(path);
                      setSavedPath(path);
                    }
                  })
                }
              >
                导出
                {completed && completed < items.length
                  ? `已完成的 ${completed} 份`
                  : "ZIP 和报告"}
              </button>
              <select
                aria-label="筛选批次文件"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
              >
                <option value="all">全部文件</option>
                <option value="pending">待检查</option>
                <option value="failed">失败</option>
                <option value="completed">已完成</option>
              </select>
            </div>
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
                  {visibleItems.map((item) => (
                    <tr key={item.index}>
                      <td>
                        <span className="file-cell">
                          <FileText size={17} />
                          <span>
                            <strong>
                              {item.display_name ||
                                `第 ${item.index + 1} 份文件`}
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
                        {checked(item) && <small>已确认检查</small>}
                      </td>
                      <td>
                        {item.error_info ? (
                          <details>
                            <summary>{item.error_info.title}</summary>
                            <p>{item.error_info.recovery_action}</p>
                          </details>
                        ) : (
                          item.error || "—"
                        )}
                      </td>
                      <td>
                        <button
                          className="secondary"
                          disabled={
                            pending ||
                            !item.task_id ||
                            !["awaiting_review", "completed"].includes(
                              item.state,
                            )
                          }
                          title={
                            item.state === "failed"
                              ? "此文件需要重新分析"
                              : undefined
                          }
                          onClick={() =>
                            void run(async () => {
                              await flushReview();
                              const store = useWorkbench.getState();
                              store.setTask(
                                await call("task_view", { id: item.task_id! }),
                              );
                              store.setBatchId(id);
                              navigate("/");
                            })
                          }
                        >
                          {item.state === "completed"
                            ? "查看结果"
                            : checked(item)
                              ? "再次检查"
                              : "打开检查"}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {!visibleItems.length && (
              <p className="empty">没有符合条件的文件。</p>
            )}
          </>
        )}
      </section>
    </>
  );
}
