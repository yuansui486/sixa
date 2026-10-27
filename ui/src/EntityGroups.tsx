import { useEffect, useMemo, useState } from "react";
import type { Entity } from "./api";
import { VirtualList } from "./VirtualList";
export function groupEntities(entities: Entity[]) {
  const groups = new Map<
    string,
    { id: string; text: string; type: string; items: Entity[] }
  >();
  for (const entity of entities) {
    const key = JSON.stringify([entity.text, entity.entity_type]);
    const group = groups.get(key) || {
      id: key,
      text: entity.text,
      type: entity.type_label,
      items: [],
    };
    group.items.push(entity);
    groups.set(key, group);
  }
  return [...groups.values()];
}
export function EntityGroups({
  entities,
  text,
  filter,
  focusedId,
  editable,
  onFocus,
  onChange,
}: {
  entities: Entity[];
  text: string;
  filter: string;
  focusedId: string | null;
  editable: boolean;
  onFocus: (id: string) => void;
  onChange: (entities: Entity[]) => void;
}) {
  const groups = useMemo(() => groupEntities(entities), [entities]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [pages, setPages] = useState<Record<string, number>>({});
  const active = groups.find((g) => g.items.some((e) => e.id === focusedId));
  useEffect(() => {
    if (active && focusedId) {
      setExpanded((current) => new Set([...current, active.id]));
      setPages((current) => ({
        ...current,
        [active.id]: Math.floor(
          active.items.findIndex((e) => e.id === focusedId) / 30,
        ),
      }));
    }
  }, [active?.id, focusedId]);
  const update = (ids: string[], patch: Partial<Entity>) => {
    const wanted = new Set(ids);
    onChange(entities.map((e) => (wanted.has(e.id) ? { ...e, ...patch } : e)));
  };
  const visible = groups.filter(
    (g) =>
      g.id === active?.id ||
      g.text.toLowerCase().includes(filter.toLowerCase()) ||
      g.type.includes(filter),
  );
  return (
    <VirtualList
      items={visible}
      focusedId={active?.id ?? null}
      render={(group) => {
        const ids = group.items.map((e) => e.id),
          selected = group.items.filter((e) => e.selected).length;
        const replacements = new Set(
          group.items.map(
            (e) => e.replacement ?? e.effective_replacement ?? "默认方式",
          ),
        );
        const opened = expanded.has(group.id);
        const start = (pages[group.id] || 0) * 30;
        return (
          <section className="entity-group">
            <div className="entity-group-heading">
              <input
                type="checkbox"
                aria-label={`选择${group.text}`}
                checked={selected === ids.length}
                ref={(element) => {
                  if (element)
                    element.indeterminate =
                      selected > 0 && selected < ids.length;
                }}
                disabled={!editable}
                onChange={(e) => update(ids, { selected: e.target.checked })}
              />
              <button
                className="entity-copy"
                onClick={() => {
                  setExpanded((current) => {
                    const next = new Set(current);
                    opened ? next.delete(group.id) : next.add(group.id);
                    return next;
                  });
                  if (!opened) onFocus(group.items[0].id);
                }}
                aria-expanded={opened}
              >
                <strong>{group.text}</strong>
                <span>
                  {group.type} · 共 {ids.length} 处 · 已选 {selected}
                </span>
              </button>
            </div>
            <details className="entity-group-replacement">
              <summary>
                替换为：
                {replacements.size === 1 ? [...replacements][0] : "多种替换"}
              </summary>
              <input
                aria-label={`${group.text}全部替换文字`}
                value={
                  replacements.size === 1
                    ? (group.items[0].replacement ?? "")
                    : ""
                }
                placeholder="为所有引用设置替换文字"
                disabled={!editable}
                onChange={(e) => update(ids, { replacement: e.target.value })}
              />
            </details>
            {opened &&
              group.items.slice(start, start + 30).map((entity, localIndex) => {
                const index = start + localIndex;
                return (
                  <div
                    className={`entity-item ${entity.id === focusedId ? "focused" : ""}`}
                    key={entity.id}
                    id={`review-${entity.id}`}
                  >
                    <input
                      type="checkbox"
                      checked={entity.selected}
                      disabled={!editable}
                      aria-label={`第${index + 1}处${entity.text}的选择状态`}
                      onChange={(e) =>
                        update([entity.id], { selected: e.target.checked })
                      }
                    />
                    <button
                      className="entity-copy entity-locate"
                      onClick={() => onFocus(entity.id)}
                    >
                      <strong>第 {index + 1} 处</strong>
                      <span>
                        {Math.round(entity.score * 100)}% · {entity.source}
                      </span>
                      <small className="entity-context">
                        {text
                          .slice(
                            Math.max(0, entity.display.start - 16),
                            entity.display.end + 22,
                          )
                          .replace(/\s+/g, " ")}
                      </small>
                    </button>
                    <details className="entity-replacement">
                      <summary>
                        {entity.replacement ??
                          entity.effective_replacement ??
                          "默认方式"}
                      </summary>
                      <input
                        disabled={!editable}
                        aria-label={`${entity.text}第${index + 1}处替换文字`}
                        value={entity.replacement ?? ""}
                        onChange={(e) =>
                          update([entity.id], { replacement: e.target.value })
                        }
                      />
                    </details>
                  </div>
                );
              })}
            {opened && group.items.length > 30 && (
              <div className="row-actions">
                <button
                  disabled={!start}
                  onClick={() =>
                    setPages((p) => ({
                      ...p,
                      [group.id]: (p[group.id] || 0) - 1,
                    }))
                  }
                >
                  上一页引用
                </button>
                <span>
                  {start + 1}–{Math.min(start + 30, group.items.length)}
                </span>
                <button
                  disabled={start + 30 >= group.items.length}
                  onClick={() =>
                    setPages((p) => ({
                      ...p,
                      [group.id]: (p[group.id] || 0) + 1,
                    }))
                  }
                >
                  下一页引用
                </button>
              </div>
            )}
          </section>
        );
      }}
    />
  );
}
