import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { call, message, type Region, type TextMatches } from "./api";
import { flushReview, applyTextMatches } from "./review";
import { useWorkbench } from "./store";

export function QuickRedaction({
  id,
  revision,
  editable,
  selectedText,
  selectionToken,
  fileName,
  onLocate,
  onPreview,
}: {
  id: string;
  revision: number;
  editable: boolean;
  selectedText: string;
  selectionToken: number;
  fileName: string;
  onLocate: (
    span: { start: number; end: number },
    page?: number | null,
  ) => void;
  onPreview: (regions: Region[], page?: number) => void;
}) {
  const [query, setQuery] = useState(selectedText),
    [replacement, setReplacement] = useState("已脱敏");
  const [matches, setMatches] = useState<TextMatches | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [saveRule, setSaveRule] = useState(false),
    [ruleName, setRuleName] = useState(""),
    [notice, setNotice] = useState("");
  const [rulePending, setRulePending] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const ruleId = useRef(crypto.randomUUID());
  const pendingRule = useRef<{
    id: string;
    name: string;
    text: string;
    replacement: string;
  } | null>(null);
  const client = useQueryClient();
  const confirmDialog = useRef<HTMLDialogElement>(null);
  const persistRule = async () => {
    if (!pendingRule.current) return;
    await call("save_quick_rule", pendingRule.current);
    pendingRule.current = null;
    ruleId.current = crypto.randomUUID();
    setRulePending(false);
    await client.invalidateQueries({ queryKey: ["rules"] });
    await client.invalidateQueries({ queryKey: ["policies"] });
  };
  useEffect(() => {
    setQuery(selectedText);
    setMatches(null);
  }, [selectedText, id, selectionToken]);
  useEffect(() => setMatches(null), [revision]);
  useEffect(() => {
    if (confirming && confirmDialog.current && !confirmDialog.current.open)
      confirmDialog.current.showModal();
    if (!confirming && confirmDialog.current?.open)
      confirmDialog.current.close();
  }, [confirming]);
  const preview = async (found: TextMatches, page?: number | null) => {
    const regions: Region[] = [];
    for (const item of found.items) {
      if (item.page !== page || !item.polygon) continue;
      regions.push({
        id: `match-${item.id}`,
        page: item.page!,
        polygon: item.polygon,
        entity_id: null,
        selected: false,
        source: "ocr",
        text: query,
        score: null,
        rotation: 0,
        replacement,
        preview_kind: "match",
      });
      for (const conflict of item.conflict_details) {
        if (conflict.page !== page || !conflict.polygon) continue;
        regions.push({
          id: `conflict-${item.id}-${regions.length}`,
          page: conflict.page!,
          polygon: conflict.polygon,
          entity_id: null,
          selected: false,
          source: "ocr",
          text: conflict.context,
          score: null,
          rotation: 0,
          replacement: conflict.replacement,
          preview_kind: "conflict",
        });
      }
    }
    onPreview(regions, page ?? undefined);
  };
  const refreshPagePreview = async (page?: number | null) => {
    if (!matches || page == null) {
      onPreview([], undefined);
      return;
    }
    const updated = await call("find_text", {
      id,
      query,
      previewPage: page,
    });
    setMatches(updated);
    await preview(updated, page);
  };
  const apply = async () => {
    if (!matches) return;
    await flushReview();
    const mutationId = crypto.randomUUID();
    await applyTextMatches(id, {
      query,
      replacement,
      match_ids: matches.items.map((m) => m.id),
      expected_revision: matches.revision,
      mutation_id: mutationId,
      allow_conflicts: matches.items.some((m) => m.conflicts > 0),
    });
    setConfirming(false);
    setMatches(null);
    onPreview([], undefined);
    setNotice(`已替换 ${matches.total} 处，可用工作台的撤销恢复。`);
    if (saveRule) {
      pendingRule.current = {
        id: ruleId.current,
        name: ruleName,
        text: query,
        replacement,
      };
      setRulePending(true);
      try {
        await persistRule();
        setNotice(`已替换 ${matches.total} 处并保存规则。`);
      } catch (e) {
        throw Error(`替换已完成，规则保存失败：${message(e)}`);
      }
    }
  };
  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    useWorkbench.setState({ manualBusy: true });
    setError("");
    try {
      await fn();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
      useWorkbench.setState({ manualBusy: false });
    }
  };
  const conflictTotal =
    matches?.items.reduce((sum, item) => sum + item.conflicts, 0) ?? 0;
  const conflictDetails =
    matches?.items.flatMap((item) => item.conflict_details) ?? [];
  return (
    <section className="quick-redaction" aria-label="快速文字脱敏">
      <h3>查找并替换</h3>
      <p className="quick-scope">处理范围：当前文件「{fileName}」</p>
      <p className="muted">
        作用于当前文件。Word 可先选中文字，图片可直接框选。
      </p>
      <label>
        查找文字
        <input
          aria-label="查找脱敏文字"
          value={query}
          disabled={!editable || busy}
          onChange={(e) => {
            setQuery(e.target.value);
            setMatches(null);
            onPreview([], undefined);
          }}
        />
      </label>
      <label>
        替换为
        <input
          aria-label="手动替换文字"
          value={replacement}
          disabled={!editable || busy}
          onChange={(e) => setReplacement(e.target.value)}
        />
      </label>
      <label className="quick-rule-toggle">
        <input
          type="checkbox"
          checked={saveRule}
          disabled={!editable || busy}
          onChange={(e) => setSaveRule(e.target.checked)}
        />
        同时保存为规则，供以后使用
      </label>
      {saveRule && (
        <input
          aria-label="快捷规则名称"
          placeholder="规则名称"
          value={ruleName}
          disabled={!editable || busy}
          onChange={(e) => setRuleName(e.target.value)}
        />
      )}
      <div className="row-actions">
        <button
          className="secondary"
          disabled={!editable || busy || !query.trim()}
          onClick={() =>
            void run(async () => {
              await flushReview();
              const found = await call("find_text", { id, query });
              const page = found.items[0]?.page;
              const withPreview =
                page == null
                  ? found
                  : await call("find_text", { id, query, previewPage: page });
              setMatches(withPreview);
              await preview(withPreview, page);
              setNotice("");
            })
          }
        >
          查找
        </button>
        <button
          disabled={
            !editable ||
            busy ||
            rulePending ||
            !matches?.items.length ||
            matches.revision !== revision ||
            matches.items.some((i) => !i.locatable) ||
            (saveRule && !ruleName.trim())
          }
          onClick={() => {
            if (matches?.items.some((item) => item.conflicts > 0))
              setConfirming(true);
            else void run(apply);
          }}
        >
          {busy ? "正在处理…" : "全部替换"}
        </button>
      </div>
      {matches && (
        <>
          <p role="status">
            当前文件共命中 {matches.total} 处
            {matches.items.some((i) => !i.locatable)
              ? "，部分位置无法可靠映射，请分行查找或框选。"
              : ""}
          </p>
          <details>
            <summary>查看匹配位置</summary>
            <ol className="quick-matches">
              {matches.items.slice(0, 100).map((item, index) => (
                <li key={item.id}>
                  <button
                    className="text-action"
                    onClick={() => {
                      onLocate(item.display, item.page);
                      void run(() => refreshPagePreview(item.page));
                    }}
                  >
                    第 {index + 1} 处
                  </button>{" "}
                  {item.context}
                  {item.conflicts > 0 &&
                    `（覆盖 ${item.conflicts} 个已有范围）`}
                </li>
              ))}
            </ol>
            {matches.items.length > 100 && (
              <p>仅展示前 100 处，全部替换作用于所有命中。</p>
            )}
          </details>
        </>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {rulePending && (
        <button
          className="secondary"
          disabled={busy}
          onClick={() =>
            void run(async () => {
              await persistRule();
              setNotice("规则已保存。");
            })
          }
        >
          重试保存规则
        </button>
      )}
      {confirming && matches && (
        <dialog
          className="save-dialog quick-conflict-dialog"
          ref={confirmDialog}
          aria-label="确认覆盖已有脱敏范围"
          onCancel={(event) => {
            event.preventDefault();
            setConfirming(false);
          }}
        >
          <h2>确认替换范围</h2>
          <p>
            当前文件「{fileName}」共命中 {matches.total} 处，其中有
            {conflictTotal}
            个已有脱敏范围会被覆盖。新替换会移除所列旧范围并应用到命中位置。
          </p>
          <div className="quick-conflict-list">
            {conflictDetails.slice(0, 20).map((detail, index) => (
              <div key={index}>
                <strong>
                  {detail.type_label} · {detail.replacement}
                </strong>
                <span>
                  {detail.page == null
                    ? "文字位置"
                    : `第 ${detail.page + 1} 页`}
                </span>
                <small>{detail.context}</small>
              </div>
            ))}
          </div>
          {conflictTotal > conflictDetails.slice(0, 20).length && (
            <p className="muted">
              冲突数量较多，仅列出前 20 个详情；全部冲突都会按新替换处理。
            </p>
          )}
          {matches.items.some((item) => item.page != null) && (
            <p className="muted">
              PDF/图片预览中，蓝色区域为新命中，红色区域为将被覆盖的旧范围。
            </p>
          )}
          <div className="save-dialog-actions">
            <button className="secondary" onClick={() => setConfirming(false)}>
              取消
            </button>
            <button disabled={busy} onClick={() => void run(apply)}>
              {busy ? "正在替换…" : "确认并替换全部命中"}
            </button>
          </div>
        </dialog>
      )}
      {!editable && (
        <p className="muted">结果已锁定，请点击“继续调整”后修改。</p>
      )}
    </section>
  );
}
