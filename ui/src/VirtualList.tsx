import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

function MeasuredRow({
  children,
  onHeight,
}: {
  children: ReactNode;
  onHeight: (height: number) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const observer = new ResizeObserver(([entry]) =>
      onHeight(entry.contentRect.height),
    );
    if (ref.current) observer.observe(ref.current);
    return () => observer.disconnect();
  }, [onHeight]);
  return <div ref={ref}>{children}</div>;
}

export function VirtualList<T extends { id: string }>({
  items,
  focusedId,
  render,
}: {
  items: T[];
  focusedId: string | null;
  render: (item: T) => ReactNode;
}) {
  const root = useRef<HTMLDivElement>(null);
  const heights = useRef(new Map<string, number>());
  const [viewport, setViewport] = useState({ top: 0, height: 600 });
  const [, setMeasured] = useState(0);
  const offsets = [0];
  for (const item of items)
    offsets.push(
      offsets[offsets.length - 1] + (heights.current.get(item.id) ?? 92),
    );
  let start = 0;
  while (start < items.length && offsets[start + 1] < viewport.top - 350)
    start++;
  let end = start;
  while (
    end < items.length &&
    offsets[end] < viewport.top + viewport.height + 350
  )
    end++;
  useLayoutEffect(() => {
    if (!root.current) return;
    const observer = new ResizeObserver(() => {
      if (root.current)
        setViewport({
          top: root.current.scrollTop,
          height: root.current.clientHeight,
        });
    });
    observer.observe(root.current);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const index = items.findIndex((item) => item.id === focusedId);
    if (index >= 0 && root.current) {
      const top = offsets[index];
      if (
        top < root.current.scrollTop ||
        top + 92 > root.current.scrollTop + root.current.clientHeight
      )
        root.current.scrollTop = Math.max(0, top - 60);
    }
  }, [focusedId, items.map((item) => item.id).join(",")]);
  return (
    <div
      className="entity-list"
      ref={root}
      onScroll={(event) =>
        setViewport({
          top: event.currentTarget.scrollTop,
          height: event.currentTarget.clientHeight,
        })
      }
    >
      <div style={{ height: offsets[start] }} />
      {items.slice(start, end).map((item) => (
        <MeasuredRow
          key={item.id}
          onHeight={(height) => {
            if (height > 0 && heights.current.get(item.id) !== height) {
              heights.current.set(item.id, height);
              setMeasured((n) => n + 1);
            }
          }}
        >
          {render(item)}
        </MeasuredRow>
      ))}
      <div style={{ height: offsets[items.length] - offsets[end] }} />
    </div>
  );
}
