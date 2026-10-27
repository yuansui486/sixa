import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { call, message, type TextMatches } from "./api";
import { flushReview, applyTextMatches } from "./review";
import { useWorkbench } from "./store";

export function QuickRedaction({
  id,
  revision,
  editable,
  selectedText,
  selectionToken,
  onLocate,
}: {
  id: string;
  revision: number;
  editable: boolean;
  selectedText: string;
  selectionToken: number;
  onLocate: (
    span: { start: number; end: number },
    page?: number | null,
  ) => void;
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
  const ruleId = useRef(crypto.randomUUID());
  const pendingRule = useRef<{
    id: string;
    name: string;
    text: string;
    replacement: string;
  } | null>(null);
  const client = useQueryClient();
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
  return (
    <section className="quick-redaction" aria-label="快速文字脱敏">
      <h3>查找并替换</h3>
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
              setMatches(await call("find_text", { id, query }));
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
          onClick={() =>
            void run(async () => {
              if (!matches) return;
              const conflict = matches.items.reduce(
                (sum, m) => sum + m.conflicts,
                0,
              );
              if (
                conflict &&
                !window.confirm(
                  `其中 ${conflict} 处与已有脱敏范围重叠，确认以本次替换为准？`,
                )
              )
                return;
              await flushReview();
              const mutationId = crypto.randomUUID();
              await applyTextMatches(id, {
                query,
                replacement,
                match_ids: matches.items.map((m) => m.id),
                expected_revision: matches.revision,
                mutation_id: mutationId,
                allow_conflicts: !!conflict,
              });
              setMatches(null);
              setNotice(
                `已替换 ${matches.items.length} 处，可用工作台的撤销恢复。`,
              );
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
                  setNotice(`已替换 ${matches.items.length} 处并保存规则。`);
                } catch (e) {
                  throw Error(`替换已完成，规则保存失败：${message(e)}`);
                }
              }
            })
          }
        >
          {busy ? "正在处理…" : "全部替换"}
        </button>
      </div>
      {matches && (
        <>
          <p role="status">
            找到 {matches.items.length} 处
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
                    onClick={() => onLocate(item.display, item.page)}
                  >
                    第 {index + 1} 处
                  </button>{" "}
                  {item.context}
                  {item.conflicts > 0 && "（与已有范围重叠）"}
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
      {!editable && (
        <p className="muted">结果已锁定，请点击“继续调整”后修改。</p>
      )}
    </section>
  );
}
