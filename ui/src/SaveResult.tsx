import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import { Folder, LoaderCircle } from "lucide-react";
import { call, message } from "./api";

export interface SaveTarget {
  id?: string;
  outputId?: string;
  name: string;
}
export interface SaveLocation {
  path: string;
  relative: string;
  external: boolean;
}
export const childPath = (relative: string, name: string) =>
  [relative, name].filter(Boolean).join("/");
export function SaveResult({
  target,
  onClose,
  onSaved,
}: {
  target: SaveTarget;
  onClose: () => void;
  onSaved: (location: SaveLocation) => void;
}) {
  const [relative, setRelative] = useState("");
  const [name, setName] = useState(target.name);
  const [folder, setFolder] = useState("");
  const [offset, setOffset] = useState(0);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const initialized = useRef(false);
  const operation = useRef({ key: "", id: crypto.randomUUID() });
  const dialog = useRef<HTMLDialogElement>(null);
  const client = useQueryClient();
  const directory = useQuery({
    queryKey: ["save-directory", relative, offset],
    queryFn: () => call("browse_directory", { query: { relative, offset } }),
  });
  const lastLocation = useQuery({
    queryKey: ["last-output-location"],
    queryFn: () => call("get_last_output_location"),
  });
  useEffect(() => {
    if (initialized.current || directory.isPending || lastLocation.isPending)
      return;
    initialized.current = true;
    if (lastLocation.data) setRelative(lastLocation.data);
  }, [directory.isPending, lastLocation.data, lastLocation.isPending]);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  const run = async (fn: () => Promise<void>) => {
    if (busy) return;
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
  const finish = async (
    path: string,
    external = false,
    savedRelative = relative,
  ) => {
    await client.invalidateQueries({ queryKey: ["output-files"] });
    await client.invalidateQueries({ queryKey: ["directory"] });
    await client.invalidateQueries({ queryKey: ["tasks"] });
    await client.invalidateQueries({ queryKey: ["last-output-location"] });
    onSaved({ path, relative: savedRelative, external });
  };
  const saveLast = () =>
    void run(async () => {
      if (!lastLocation.data) return;
      const key = `last:${lastLocation.data}:${name}`;
      if (operation.current.key !== key)
        operation.current = { key, id: crypto.randomUUID() };
      const mutationId = operation.current.id;
      const result = target.outputId
        ? await call("copy_result_to_last", {
            id: target.outputId,
            name,
            mutationId,
          })
        : await call("save_result_to_last", {
            id: target.id!,
            name,
            mutationId,
          });
      setRelative(lastLocation.data);
      await finish(result.path, false, lastLocation.data);
    });
  return createPortal(
    <dialog
      className="save-dialog"
      ref={dialog}
      aria-label="保存脱敏文件"
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
    >
      <h2>保存脱敏文件</h2>
      <p className="muted">选择保存位置，已有文件不会被覆盖。</p>
      {!lastLocation.data && !lastLocation.isPending && (
        <p className="hint-banner">
          首次保存请选择文件夹；私匣会记住成功使用的位置。
        </p>
      )}
      <p className="file-path">{directory.data?.path ?? "正在读取目录…"}</p>
      <div className="row-actions">
        <button
          className="secondary"
          disabled={busy || !relative}
          onClick={() => {
            setRelative(relative.split(/[\\/]/).slice(0, -1).join("/"));
            setOffset(0);
          }}
        >
          返回上级
        </button>
        <button
          className="text-action"
          disabled={busy}
          onClick={() => {
            setRelative("");
            setOffset(0);
          }}
        >
          保存根目录
        </button>
      </div>
      <div className="save-folders">
        {directory.data?.items
          .filter((item) => item.directory)
          .map((item) => (
            <button
              className="secondary"
              key={item.path}
              disabled={busy}
              onClick={() => {
                setRelative(childPath(relative, item.name));
                setOffset(0);
              }}
            >
              <Folder size={16} />
              {item.name}
            </button>
          ))}
        {directory.data &&
          !directory.data.items.some((item) => item.directory) && (
            <p className="muted">
              此页没有子文件夹，可保存在当前目录或新建文件夹。
            </p>
          )}
      </div>
      {directory.data && directory.data.total > 30 && (
        <div className="row-actions">
          <button
            disabled={busy || !offset}
            onClick={() => setOffset(offset - 30)}
          >
            上一页
          </button>
          <button
            disabled={busy || offset + 30 >= directory.data.total}
            onClick={() => setOffset(offset + 30)}
          >
            下一页
          </button>
        </div>
      )}
      <form
        className="row-actions"
        onSubmit={(e) => {
          e.preventDefault();
          void run(async () => {
            await call("create_output_directory", { relative, name: folder });
            setRelative(childPath(relative, folder));
            setOffset(0);
            setFolder("");
          });
        }}
      >
        <input
          aria-label="新文件夹名称"
          placeholder="新文件夹名称"
          value={folder}
          onChange={(e) => setFolder(e.target.value)}
          disabled={busy}
        />
        <button className="secondary" disabled={busy || !folder.trim()}>
          新建文件夹
        </button>
      </form>
      <label>
        文件名
        <input
          aria-label="保存文件名"
          value={name}
          disabled={busy}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      {(error || directory.error) && (
        <p className="error" role="alert">
          {error || message(directory.error)}
        </p>
      )}
      <div className="save-dialog-actions">
        {lastLocation.data && (
          <button
            className="secondary"
            disabled={busy || !name.trim()}
            onClick={saveLast}
          >
            {busy && <LoaderCircle className="spin" size={16} />}
            保存到上次位置
          </button>
        )}
        <button
          className="text-action"
          disabled={busy || !directory.data}
          onClick={() =>
            void run(async () => {
              const path = await save({
                defaultPath: `${directory.data!.root}/${name}`,
              });
              if (!path) return;
              if (target.outputId)
                await call("copy_output_file", { id: target.outputId, path });
              else await call("export_task", { id: target.id!, path });
              await finish(path, true);
            })
          }
        >
          另存到其他位置
        </button>
        <button className="secondary" disabled={busy} onClick={onClose}>
          取消
        </button>
        <button
          disabled={busy || !directory.data || !name.trim()}
          onClick={() =>
            void run(async () => {
              const key = `${relative}:${name}`;
              if (operation.current.key !== key)
                operation.current = { key, id: crypto.randomUUID() };
              if (target.outputId) {
                const result = await call("copy_result", {
                  id: target.outputId,
                  relative,
                  name,
                  mutationId: operation.current.id,
                });
                await finish(result.path);
              } else {
                const result = await call("save_result", {
                  id: target.id!,
                  relative,
                  name,
                  mutationId: operation.current.id,
                });
                await finish(result.path);
              }
            })
          }
        >
          {busy && <LoaderCircle className="spin" size={16} />}保存到当前文件夹
        </button>
      </div>
    </dialog>,
    document.body,
  );
}
export function resultName(name: string | undefined, extension: string) {
  return `${(name || "文件").replace(/\.[^.]+$/, "")}_脱敏.${extension === "batch" ? "zip" : extension}`;
}
