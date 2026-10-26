import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, Link } from "react-router-dom";
import { open, save } from "@tauri-apps/plugin-dialog";
import { FolderOpen, RefreshCw, LoaderCircle, FileCheck2 } from "lucide-react";
import { call, formatBytes, message, type OutputFile } from "./api";
import { flushReview } from "./review";
import { useWorkbench } from "./store";

export function FileSaveNotice({ taskId }: { taskId: string }) {
  const files = useQuery({
    queryKey: ["output-files", "task", taskId],
    queryFn: () => call("query_output_files", { query: { task_id: taskId } }),
  });
  const record = files.data?.items.find((file) => file.source === "automatic");
  if (files.error)
    return (
      <p className="error" role="alert">
        无法确认自动保存状态，请在<Link to="/files">文件管理</Link>中检查。
      </p>
    );
  if (!record) return null;
  return (
    <div
      className={record.error ? "error export-notice" : "export-notice"}
      role={record.error ? "alert" : "status"}
    >
      <span>
        {record.error
          ? record.error
          : record.exists
            ? `已自动保存：${record.name}`
            : "已保存的文件被移动或删除"}
      </span>
      <Link to="/files">
        {record.error ? "更换目录或重试保存" : "查看文件"}
      </Link>
    </div>
  );
}

export function Files() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const [search, setSearch] = useState("");
  const [settled, setSettled] = useState("");
  const [kind, setKind] = useState("");
  const [offset, setOffset] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => {
      setSettled(search.trim());
      setOffset(0);
    }, 250);
    return () => clearTimeout(timer);
  }, [search]);
  const directory = useQuery({
    queryKey: ["file-directory"],
    queryFn: () => call("get_file_directory"),
  });
  const files = useQuery({
    queryKey: ["output-files", settled, kind, offset],
    queryFn: () =>
      call("query_output_files", { query: { search: settled, kind, offset } }),
    refetchOnWindowFocus: true,
  });
  const run = async (id: string, action: () => Promise<void>) => {
    setBusy(id);
    setError("");
    setNotice("");
    try {
      await action();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };
  const preview = async (file: OutputFile) => {
    await flushReview();
    if (file.kind === "zip") {
      navigate(`/batch?id=${file.task_id}`);
      return;
    }
    const task = await call("output_task_view", { id: file.id });
    useWorkbench.getState().setTask(task);
    useWorkbench.getState().setBatchId(task.meta.parent_batch_id ?? null);
    navigate("/");
  };
  return (
    <>
      <header>
        <h1>文件管理</h1>
        <p>生成后的文件自动保存到这里，也可以随时另存为。</p>
      </header>
      <section className="card file-directory">
        <div>
          <h2>保存位置</h2>
          <p className="file-path" title={directory.data}>
            {directory.data ?? "正在读取…"}
          </p>
          <p className="muted">
            修改位置只影响后续保存；已有文件保留在原目录。
          </p>
        </div>
        <button
          className="secondary"
          disabled={!!busy}
          onClick={() =>
            void run("directory", async () => {
              const path = await open({
                directory: true,
                multiple: false,
                title: "选择处理结果保存位置",
              });
              if (typeof path !== "string") return;
              await call("set_file_directory", { path });
              client.setQueryData(["file-directory"], path);
              setNotice("保存位置已更新");
            })
          }
        >
          <FolderOpen size={17} />
          更改位置
        </button>
      </section>
      {(error || directory.error || files.error) && (
        <p className="error" role="alert">
          {error || message(directory.error || files.error)}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      <section className="card">
        <div className="history-tools">
          <input
            aria-label="搜索处理结果"
            placeholder="搜索文件名"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <select
            aria-label="筛选文件格式"
            value={kind}
            onChange={(e) => {
              setKind(e.target.value);
              setOffset(0);
            }}
          >
            <option value="">全部格式</option>
            {[
              "pdf",
              "docx",
              "xlsx",
              "xlsm",
              "png",
              "jpg",
              "jpeg",
              "bmp",
              "tif",
              "tiff",
              "txt",
              "md",
              "zip",
            ].map((format) => (
              <option key={format} value={format}>
                {format.toUpperCase()}
              </option>
            ))}
          </select>
          <span>{files.data?.total ?? 0} 个文件</span>
          <button
            className="secondary"
            disabled={files.isFetching}
            onClick={() => void files.refetch()}
          >
            <RefreshCw size={16} className={files.isFetching ? "spin" : ""} />
            刷新
          </button>
        </div>
        {files.isPending ? (
          <p className="empty" role="status">
            <LoaderCircle className="spin" size={18} /> 正在读取文件…
          </p>
        ) : !files.data?.items.length ? (
          <div className="empty">
            <FileCheck2 size={28} />
            <p>
              {settled || kind ? "没有符合条件的文件" : "还没有保存的处理结果"}
            </p>
            <p>
              已有任务可在<Link to="/history">任务历史</Link>中打开并保存。
            </p>
          </div>
        ) : (
          <div className="table-scroll">
            <table className="files-table">
              <thead>
                <tr>
                  <th>文件与位置</th>
                  <th>生成时间</th>
                  <th>大小</th>
                  <th>状态</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {files.data.items.map((file) => (
                  <tr key={file.id}>
                    <td>
                      <strong>{file.name}</strong>
                      <small className="file-path" title={file.path}>
                        {file.path}
                      </small>
                      {file.source === "mcp" && <small>AI 工具生成</small>}
                    </td>
                    <td>
                      {new Date(file.created_at * 1000).toLocaleString("zh-CN")}
                    </td>
                    <td>{file.error ? "—" : formatBytes(file.size)}</td>
                    <td>
                      {file.error ? (
                        <details className="task-error">
                          <summary>保存失败</summary>
                          <p>{file.error}</p>
                        </details>
                      ) : !file.exists ? (
                        "已移动或删除"
                      ) : file.partial ? (
                        `部分成功 · ${file.failed_count} 项未完成`
                      ) : (
                        "已保存"
                      )}
                    </td>
                    <td>
                      <div className="row-actions">
                        {busy === file.id && (
                          <LoaderCircle
                            size={16}
                            className="spin"
                            aria-label="正在处理"
                          />
                        )}
                        <button
                          className="secondary"
                          disabled={!!busy || !file.task_exists}
                          onClick={() => void run(file.id, () => preview(file))}
                        >
                          {file.kind === "zip" ? "查看批次" : "预览"}
                        </button>
                        <button
                          className="secondary"
                          disabled={!!busy || !file.exists}
                          onClick={() =>
                            void run(file.id, async () => {
                              await call("open_output_file", { id: file.id });
                            })
                          }
                        >
                          打开文件
                        </button>
                        <button
                          className="secondary"
                          disabled={!!busy || !file.exists}
                          onClick={() =>
                            void run(file.id, async () => {
                              await call("reveal_file", { path: file.path });
                            })
                          }
                        >
                          所在文件夹
                        </button>
                        {file.error ? (
                          <button
                            disabled={!!busy || !file.task_exists}
                            onClick={() =>
                              void run(file.id, async () => {
                                const result = await call("retry_output_file", {
                                  id: file.id,
                                });
                                await files.refetch();
                                if (result.error) throw new Error(result.error);
                                setNotice("文件已保存");
                              })
                            }
                          >
                            重试保存
                          </button>
                        ) : (
                          <button
                            className="text-action"
                            disabled={!!busy || !file.exists}
                            onClick={() =>
                              void run(file.id, async () => {
                                const path = await save({
                                  defaultPath: file.name,
                                });
                                if (!path) return;
                                await call("copy_output_file", {
                                  id: file.id,
                                  path,
                                });
                                await files.refetch();
                                setNotice("文件已另存");
                              })
                            }
                          >
                            另存为
                          </button>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="pagination">
          <button
            className="secondary"
            disabled={offset === 0 || !!busy}
            onClick={() => setOffset(Math.max(0, offset - 30))}
          >
            上一页
          </button>
          <span>第 {Math.floor(offset / 30) + 1} 页</span>
          <button
            className="secondary"
            disabled={offset + 30 >= (files.data?.total ?? 0) || !!busy}
            onClick={() => setOffset(offset + 30)}
          >
            下一页
          </button>
        </div>
      </section>
    </>
  );
}
