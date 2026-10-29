import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, Link, useSearchParams } from "react-router-dom";
import { open } from "@tauri-apps/plugin-dialog";
import { Folder, RefreshCw, LoaderCircle } from "lucide-react";
import { call, formatBytes, message, type DirectoryEntry } from "./api";
import { flushReview } from "./review";
import { useWorkbench } from "./store";
import { SaveResult, childPath, type SaveTarget } from "./SaveResult";
export function FileSaveNotice({ taskId }: { taskId: string }) {
  const [openError, setOpenError] = useState("");
  const files = useQuery({
    queryKey: ["output-files", "task", taskId],
    queryFn: () => call("query_output_files", { query: { task_id: taskId } }),
  });
  const record =
    files.data?.items.find((file) => !file.error) ?? files.data?.items[0];
  if (files.error)
    return (
      <p className="error" role="alert">
        无法确认保存状态，请在<Link to="/files">文件管理</Link>中检查。
      </p>
    );
  if (files.isPending) return null;
  if (!record)
    return <p className="export-notice">预览已生成，尚未保存到文件夹。</p>;
  return (
    <div
      className={record.error ? "error export-notice" : "export-notice"}
      role={record.error ? "alert" : "status"}
    >
      <span>
        {record.error
          ? record.error
          : record.exists
            ? `已保存：${record.name}`
            : "已保存的文件被移动或删除"}
      </span>
      {record.exists && !record.error ? (
        <button
          className="text-action"
          onClick={() => {
            setOpenError("");
            void call("reveal_file", { path: record.path }).catch((e) =>
              setOpenError(message(e)),
            );
          }}
        >
          所在文件夹
        </button>
      ) : (
        <Link to="/history">到任务历史重试保存</Link>
      )}
      {openError && (
        <span role="alert" className="error">
          {openError}
        </span>
      )}
    </div>
  );
}

export function Files() {
  const client = useQueryClient(),
    navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const relative = params.get("dir") || "";
  const [search, setSearch] = useState(""),
    [settled, setSettled] = useState(""),
    [kind, setKind] = useState("");
  const [offset, setOffset] = useState(0),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [notice, setNotice] = useState("");
  const [folder, setFolder] = useState(""),
    [saving, setSaving] = useState<SaveTarget | null>(null);
  useEffect(() => {
    const timer = setTimeout(() => {
      setSettled(search.trim());
      setOffset(0);
    }, 250);
    return () => clearTimeout(timer);
  }, [search]);
  const files = useQuery({
    queryKey: ["directory", relative, settled, kind, offset],
    queryFn: () =>
      call("browse_directory", {
        query: { relative, search: settled, kind, offset },
      }),
    refetchOnWindowFocus: true,
  });
  const enter = (path: string) => {
    setParams(path ? { dir: path } : {});
    setOffset(0);
    setSearch("");
  };
  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const preview = async (item: DirectoryEntry) => {
    await flushReview();
    if (item.output?.task_exists) {
      if (item.output.kind === "zip") {
        navigate(`/batch?id=${item.output.task_id}`);
        return;
      }
      useWorkbench
        .getState()
        .setTask(await call("output_task_view", { id: item.output.id }));
    } else {
      const task = await call("analyze_file", {
        path: item.path,
        options: { ocr_profile: "mobile", pdf_mode: "safe_rebuild" },
        requestId: crypto.randomUUID(),
      });
      useWorkbench.getState().setTask(task);
    }
    useWorkbench.getState().setBatchId(null);
    navigate("/");
  };
  return (
    <>
      <header>
        <h1>文件管理</h1>
        <p>浏览保存目录，查看文件或继续处理。</p>
      </header>
      <section className="card file-directory">
        <div>
          <h2>保存根目录</h2>
          <p className="file-path">{files.data?.root || "正在读取…"}</p>
          <p className="muted">
            更改位置不会移动已有文件，旧结果仍可从任务历史查询。
          </p>
        </div>
        <button
          className="secondary"
          disabled={busy}
          onClick={() =>
            void run(async () => {
              const path = await open({
                directory: true,
                multiple: false,
                title: "选择保存根目录",
              });
              if (typeof path !== "string") return;
              await call("set_file_directory", { path });
              enter("");
              await client.invalidateQueries({ queryKey: ["directory"] });
              await client.invalidateQueries({ queryKey: ["file-directory"] });
            })
          }
        >
          更改位置
        </button>
      </section>
      {(error || files.error) && (
        <p className="error" role="alert">
          {error || message(files.error)}
        </p>
      )}
      {notice && (
        <p role="status" className="export-notice">
          {notice}
        </p>
      )}
      <section className="card">
        <nav className="directory-breadcrumb" aria-label="文件夹路径">
          <button className="text-action" onClick={() => enter("")}>
            保存根目录
          </button>
          {relative
            .split(/[\\/]/)
            .filter(Boolean)
            .map((part, index, parts) => (
              <span key={index}>
                {" "}
                /{" "}
                <button
                  className="text-action"
                  onClick={() => enter(parts.slice(0, index + 1).join("/"))}
                >
                  {part}
                </button>
              </span>
            ))}
        </nav>
        <form
          className="history-tools"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              await call("create_output_directory", { relative, name: folder });
              setFolder("");
              await files.refetch();
            });
          }}
        >
          <input
            aria-label="新文件夹名称"
            placeholder="新文件夹名称"
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
          />
          <button className="secondary" disabled={busy || !folder.trim()}>
            新建文件夹
          </button>
        </form>
        <div className="history-tools">
          <input
            aria-label="搜索处理结果"
            placeholder="搜索当前文件夹"
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
            ].map((k) => (
              <option key={k} value={k}>
                {k.toUpperCase()}
              </option>
            ))}
          </select>
          <span>{files.data?.total || 0} 项</span>
          <button
            className="secondary"
            disabled={files.isFetching}
            onClick={() => void files.refetch()}
          >
            <RefreshCw size={16} />
            刷新
          </button>
        </div>
        {files.isPending ? (
          <p role="status">
            <LoaderCircle className="spin" />
            正在读取文件…
          </p>
        ) : !files.data?.items.length ? (
          <p className="empty">当前文件夹没有符合条件的文件。</p>
        ) : (
          <div className="table-scroll">
            <table className="files-table">
              <thead>
                <tr>
                  <th>文件与位置</th>
                  <th>修改时间</th>
                  <th>大小</th>
                  <th>来源</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {files.data.items.map((item) => (
                  <tr
                    key={item.path}
                    className={
                      params.get("highlight") === item.name
                        ? "file-highlight"
                        : ""
                    }
                  >
                    <td>
                      {item.directory ? (
                        <button
                          className="text-action"
                          onClick={() => enter(childPath(relative, item.name))}
                        >
                          <Folder size={17} />
                          {item.name}
                        </button>
                      ) : (
                        <strong>{item.name}</strong>
                      )}
                      <small className="file-path" title={item.path}>
                        {item.path}
                      </small>
                    </td>
                    <td>
                      {new Date(item.modified * 1000).toLocaleString("zh-CN")}
                    </td>
                    <td>{item.directory ? "—" : formatBytes(item.size)}</td>
                    <td>
                      {item.directory
                        ? "文件夹"
                        : item.output
                          ? item.output.source === "mcp"
                            ? "AI 工具生成"
                            : "私匣生成"
                          : "本地文件"}
                    </td>
                    <td>
                      <div className="row-actions">
                        {!item.directory && (
                          <>
                            <button
                              className="secondary"
                              disabled={busy}
                              onClick={() =>
                                void run(() =>
                                  call("open_directory_entry", {
                                    relative: childPath(relative, item.name),
                                  }),
                                )
                              }
                            >
                              打开文件
                            </button>
                            {(item.output?.task_exists ||
                              /\.(pdf|docx|xlsx|xlsm|png|jpe?g|bmp|tiff?|txt|md)$/i.test(
                                item.name,
                              )) && (
                              <button
                                className="secondary"
                                disabled={busy}
                                onClick={() => void run(() => preview(item))}
                              >
                                {item.output?.task_exists
                                  ? "查看任务"
                                  : "新建脱敏任务"}
                              </button>
                            )}
                            {item.output && (
                              <button
                                className="text-action"
                                disabled={busy}
                                onClick={() =>
                                  setSaving({
                                    outputId: item.output!.id,
                                    name: item.name,
                                  })
                                }
                              >
                                另存为
                              </button>
                            )}
                          </>
                        )}
                        <button
                          className="secondary"
                          disabled={busy}
                          onClick={() =>
                            void run(() =>
                              call("open_directory_entry", {
                                relative: item.directory
                                  ? childPath(relative, item.name)
                                  : relative,
                              }),
                            )
                          }
                        >
                          {item.directory ? "打开文件夹" : "所在文件夹"}
                        </button>
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
            disabled={!offset || busy}
            onClick={() => setOffset(offset - 30)}
          >
            上一页
          </button>
          <span>第 {offset / 30 + 1} 页</span>
          <button
            disabled={offset + 30 >= (files.data?.total || 0) || busy}
            onClick={() => setOffset(offset + 30)}
          >
            下一页
          </button>
        </div>
      </section>
      {saving && (
        <SaveResult
          target={saving}
          onClose={() => setSaving(null)}
          onSaved={(where) => {
            setSaving(null);
            setNotice(`已保存：${where.path}`);
            if (!where.external) enter(where.relative);
            void files.refetch();
          }}
        />
      )}
    </>
  );
}
