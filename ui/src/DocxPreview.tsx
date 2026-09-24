import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { LoaderCircle } from "lucide-react";
import {
  message,
  type DocumentPreview,
  type Entity,
  type TaskView,
} from "./api";
import {
  draftImages,
  originalImages,
  type ImageOriginals,
} from "./docx-preview-images";
import {
  annotate,
  bindAnchors,
  bindImages,
  contentChunks,
  sanitizeRendered,
  textParts,
  type AnchorMap,
  type DocxScrollAnchor,
  type OfficeAnchor,
  type OfficePreview,
} from "./docx-preview-model";
import "./docx-preview.css";

export type { DocxScrollAnchor } from "./docx-preview-model";
export interface DocxPreviewProps {
  task: TaskView;
  result: boolean;
  completed: boolean;
  focusedId?: string | null;
  onFocusEntity: (id: string) => void;
  onOpenImage: (index: number) => void;
  zoom?: number;
  fitMode?: "width" | "page";
  onPageChange?: (page: number) => void;
  onPageCount?: (count: number) => void;
  onScrollAnchor?: (anchor: DocxScrollAnchor) => void;
  scrollAnchor?: DocxScrollAnchor | null;
  draftReady?: boolean;
  draftRevision?: number;
  imagePages?: DocumentPreview["pages"];
  pageRequest?: { page: number; token: number };
}
type Session = {
  body: HTMLDivElement;
  bindings: AnchorMap;
  images: ImageOriginals;
  preview: OfficePreview;
  sourceAnchors: OfficeAnchor[];
  dispose: () => void;
};
const SHADOW_CSS = `
:host{display:block;overflow:auto;contain:layout paint style;background:#e9eceb;color:#242c28;font:14px/1.6 system-ui,sans-serif;}
.sixa-docx-pages{padding:12px;min-width:0;}
.sixa-docx-pages .sixa-word-wrapper{padding:0;background:transparent;}
.sixa-docx-pages section.sixa-word{margin:0 auto 16px;box-shadow:0 1px 4px #0002;}
.sixa-docx-entity{color:inherit;background:transparent;cursor:pointer;border-radius:2px;text-decoration:underline;text-decoration-color:#8a9690;text-underline-offset:3px;}
.sixa-docx-entity.is-selected{background:#cce5d6;text-decoration:none;}
.sixa-docx-entity.is-focused{outline:2px solid #247247;outline-offset:2px;}
.sixa-docx-entity:focus-visible,.sixa-docx-image:focus-visible{outline:2px solid #247247;outline-offset:3px;}
.sixa-docx-image{cursor:pointer;}
.sixa-docx-image:hover{outline:2px solid #247247;outline-offset:3px;}
.sixa-docx-image-state{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;padding:6px;background:#eef2ef;color:#42574a;font:12px/1.5 system-ui,sans-serif;text-align:center;z-index:2;}
a{color:inherit;cursor:default;text-decoration:inherit;}
`;

/** The source DOM survives entity edits and source/draft toggles. Output uses its own DOCX. */
export function DocxPreview(props: DocxPreviewProps) {
  const {
    task,
    result,
    completed,
    focusedId,
    zoom = 1,
    fitMode = "width",
  } = props;
  const documentResult = result && completed;
  const revisionKey = documentResult ? task.revision : 0;
  const host = useRef<HTMLDivElement>(null);
  const current = useRef(props);
  current.current = props;
  const session = useRef<Session | null>(null);
  const suppressScrollUntil = useRef(0);
  const [preview, setPreview] = useState<OfficePreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [layoutReason, setLayoutReason] = useState("");
  const [retry, setRetry] = useState(0);
  const loadKey = `${task.meta.id}:${documentResult}:${revisionKey}:${retry}`;
  const [loadedKey, setLoadedKey] = useState("");
  const busy = loading || loadedKey !== loadKey;
  const [rendered, setRendered] = useState(0);
  const [otherAnchors, setOtherAnchors] = useState<OfficeAnchor[]>([]);
  const [otherOpen, setOtherOpen] = useState(false);
  const [visiblePage, setVisiblePage] = useState(0);
  const [imageRetry, setImageRetry] = useState(0);

  useEffect(() => {
    const target = host.current;
    if (!target) return;
    const shadow = target.shadowRoot ?? target.attachShadow({ mode: "open" });
    let disposed = false;
    let loaded: Session | null = null;
    let metadata: OfficePreview | null = null;
    let observer: IntersectionObserver | undefined;
    const body = document.createElement("div");
    body.className = "sixa-docx-pages";
    const styles = document.createElement("div");
    const urls = new Set<string>();
    const cleanup = () => {
      observer?.disconnect();
      urls.forEach((url) => URL.revokeObjectURL(url));
      urls.clear();
      body.replaceChildren();
      styles.replaceChildren();
    };
    setPreview(null);
    setError("");
    setLayoutReason("");
    setOtherAnchors([]);
    setOtherOpen(false);
    setLoading(true);
    setVisiblePage(0);
    shadow.replaceChildren();
    session.current = null;
    const args = {
      id: task.meta.id,
      result: documentResult,
      expectedRevision: current.current.task.revision,
    };
    const activate = (event: Event) => {
      if (event instanceof KeyboardEvent && !["Enter", " "].includes(event.key))
        return;
      const element =
        event.target instanceof Element
          ? event.target.closest("[data-entity-id],[data-image-index],a")
          : null;
      if (!element) return;
      event.preventDefault();
      event.stopPropagation();
      const entityId = element.getAttribute("data-entity-id");
      const imageIndex = element.getAttribute("data-image-index");
      if (entityId) current.current.onFocusEntity(entityId);
      else if (imageIndex !== null)
        current.current.onOpenImage(Number(imageIndex));
    };
    shadow.addEventListener("click", activate, true);
    shadow.addEventListener("auxclick", activate, true);
    shadow.addEventListener("keydown", activate, true);
    void (async () => {
      try {
        const meta = await invoke<OfficePreview>("office_preview", args);
        if (disposed) return;
        setPreview(meta);
        metadata = meta;
        if (!meta.layout_available) {
          setLayoutReason(
            meta.reason || "此文档使用分块内容视图，所有可提取内容仍可复核。",
          );
          current.current.onPageCount?.(0);
          return;
        }
        const raw = await invoke<ArrayBuffer | number[]>(
          "office_preview_docx",
          args,
        );
        if (disposed) return;
        const { renderAsync } = await import("docx-preview");
        if (disposed) return;
        // Detached output is sanitized before any styles enter the application DOM.
        await renderAsync(
          raw instanceof ArrayBuffer
            ? new Uint8Array(raw)
            : new Uint8Array(raw),
          body,
          styles,
          {
            className: "sixa-word",
            inWrapper: true,
            breakPages: true,
            renderAltChunks: false,
            renderChanges: false,
            ignoreFonts: true,
            useBase64URL: false,
            renderComments: false,
            experimental: false,
            renderHeaders: true,
            renderFooters: true,
            renderFootnotes: true,
            renderEndnotes: true,
          },
        );
        sanitizeRendered(body, styles).forEach((url) => urls.add(url));
        if (disposed) {
          cleanup();
          return;
        }
        if (body.querySelectorAll("*").length > 10_000) {
          cleanup();
          setLayoutReason("文档内容较多，已切换为完整分块视图，避免预览卡顿。");
          current.current.onPageCount?.(0);
          return;
        }
        const bindings = bindAnchors(body, meta);
        bindImages(body, meta);
        const base = document.createElement("style");
        base.textContent = SHADOW_CSS;
        shadow.replaceChildren(styles, base, body);
        loaded = {
          body,
          bindings,
          images: originalImages(body),
          preview: meta,
          sourceAnchors: meta.anchors,
          dispose: cleanup,
        };
        session.current = loaded;
        setRendered((value) => value + 1);
        const pages = [
          ...body.querySelectorAll<HTMLElement>("section.sixa-word"),
        ];
        current.current.onPageCount?.(pages.length);
        if (typeof IntersectionObserver !== "undefined") {
          observer = new IntersectionObserver(
            (records) => {
              const visible = records
                .filter((record) => record.isIntersecting)
                .sort((a, b) => b.intersectionRatio - a.intersectionRatio)[0];
              if (!visible) return;
              const page = pages.indexOf(visible.target as HTMLElement);
              if (page < 0) return;
              setVisiblePage(page);
              current.current.onPageChange?.(page);
            },
            { threshold: [0, 0.2, 0.5] },
          );
          pages.forEach((page) => observer?.observe(page));
        }
        // Output text has its own offsets. Source metadata is used only to link
        // entity focus to a stable bookmark, never to rewrite generated output.
        if (documentResult) {
          void invoke<OfficePreview>("office_preview", {
            ...args,
            result: false,
          })
            .then((source) => {
              if (!disposed && loaded && session.current === loaded) {
                loaded.sourceAnchors = source.anchors;
                setRendered((value) => value + 1);
              }
            })
            .catch(() => {
              /* Generated output remains valid without optional focus linkage. */
            });
        }
      } catch (cause) {
        if (disposed) return;
        cleanup();
        session.current = null;
        shadow.replaceChildren();
        // A failed output read must never be presented as a successful source preview.
        if (documentResult)
          setError(`无法读取已生成的 DOCX：${message(cause)}`);
        else if (!metadata) setError(`无法读取文档内容：${message(cause)}`);
        else
          setLayoutReason(
            `排版预览暂不可用：${message(cause)}。下方保留完整分块内容。`,
          );
      } finally {
        if (!disposed) {
          setLoading(false);
          setLoadedKey(loadKey);
        }
      }
    })();
    return () => {
      disposed = true;
      shadow.removeEventListener("click", activate, true);
      shadow.removeEventListener("auxclick", activate, true);
      shadow.removeEventListener("keydown", activate, true);
      cleanup();
      if (session.current === loaded) session.current = null;
      shadow.replaceChildren();
    };
  }, [task.meta.id, documentResult, revisionKey, retry]);

  useLayoutEffect(() => {
    const active = session.current;
    if (!active) return;
    return draftImages({
      body: active.body,
      originals: active.images,
      task,
      enabled: result && !documentResult,
      ready: !!props.draftReady && props.draftRevision === task.revision,
      revision: props.draftRevision ?? task.revision,
      page: visiblePage,
      imagePages: props.imagePages,
      retry: () => setImageRetry((value) => value + 1),
    });
  }, [
    rendered,
    task.entities,
    task.regions,
    task.revision,
    result,
    documentResult,
    props.draftReady,
    props.draftRevision,
    props.imagePages,
    visiblePage,
    imageRetry,
  ]);

  useLayoutEffect(() => {
    const active = session.current;
    if (!active) return;
    const incomplete = documentResult
      ? []
      : annotate(active.bindings, task.entities, result, focusedId);
    const ids = new Set(incomplete);
    const incompleteEntities = task.entities
      .filter((entity) => ids.has(entity.id))
      .sort((a, b) => a.display.start - b.display.start);
    const remaining = active.preview.anchors.filter((anchor) => {
      if (!active.bindings.has(anchor.id)) return true;
      let left = 0,
        right = incompleteEntities.length;
      while (left < right) {
        const mid = (left + right) >>> 1;
        if (incompleteEntities[mid].display.end <= anchor.display.start)
          left = mid + 1;
        else right = mid;
      }
      return (
        left < incompleteEntities.length &&
        overlaps(anchor, incompleteEntities[left])
      );
    });
    setOtherAnchors(remaining);
  }, [rendered, task.entities, result, documentResult, focusedId]);

  useEffect(() => {
    const active = session.current;
    if (!active || !focusedId) return;
    const entity = task.entities.find((item) => item.id === focusedId);
    if (!entity) return;
    const anchor = active.sourceAnchors.find((anchor) =>
      overlaps(anchor, entity),
    );
    const binding = anchor && active.bindings.get(anchor.id)?.[0];
    if (binding && host.current)
      scrollMarker(host.current, binding.marker, -120);
    else setOtherOpen(true);
  }, [focusedId, rendered, task.entities]);

  useEffect(() => {
    const anchor = props.scrollAnchor;
    const active = session.current;
    const target = host.current;
    if (!anchor || !active || !target) return;
    const pages = [...active.body.querySelectorAll("section.sixa-word")];
    const choices = active.bindings.get(anchor.anchorId);
    const marker =
      choices?.find(
        (binding) =>
          pages.indexOf(binding.marker.closest("section.sixa-word")!) ===
          anchor.page,
      )?.marker ?? choices?.[0]?.marker;
    if (!marker) return;
    const distance =
      anchor.fraction !== undefined
        ? Math.max(0, Math.min(1, anchor.fraction)) *
          anchorHeight(marker, active.bindings)
        : Math.max(0, anchor.offset ?? 0);
    suppressScrollUntil.current = performance.now() + 100;
    scrollMarker(target, marker, distance);
  }, [props.scrollAnchor, rendered]);

  useEffect(() => {
    const page = props.pageRequest?.page;
    const active = session.current;
    if (page === undefined || !active || !host.current) return;
    const pages = [
      ...active.body.querySelectorAll<HTMLElement>("section.sixa-word"),
    ];
    const index = Math.max(0, Math.min(page, pages.length - 1));
    if (pages[index]) {
      scrollMarker(host.current, pages[index], 0);
      setVisiblePage(index);
      current.current.onPageChange?.(index);
    }
  }, [props.pageRequest?.token, props.pageRequest?.page, rendered]);

  useEffect(() => {
    const active = session.current;
    const target = host.current;
    if (!active || !target) return;
    const scroller = scrollContainer(target);
    let frame = 0;
    let last = "";
    const update = () => {
      frame = 0;
      if (performance.now() < suppressScrollUntil.current) return;
      const top = scroller.getBoundingClientRect().top + 8;
      const pages = [
        ...active.body.querySelectorAll<HTMLElement>("section.sixa-word"),
      ];
      const page = Math.max(
        0,
        pages.findIndex((page) => page.getBoundingClientRect().bottom > top),
      );
      const markers = [
        ...(pages[page] ?? active.body).querySelectorAll<HTMLElement>("[id]"),
      ].filter((marker) => active.bindings.has(marker.id));
      const marker =
        markers
          .filter((marker) => marker.getBoundingClientRect().top <= top)
          .at(-1) ?? markers[0];
      if (!marker) return;
      const offset = Math.max(0, top - marker.getBoundingClientRect().top);
      const fraction = Math.max(
        0,
        Math.min(1, offset / anchorHeight(marker, active.bindings)),
      );
      const key = `${marker.id}:${page}:${Math.round(fraction * 1000)}`;
      if (key === last) return;
      last = key;
      setVisiblePage(page);
      current.current.onPageChange?.(page);
      current.current.onScrollAnchor?.({
        anchorId: marker.id,
        page,
        offset,
        fraction,
      });
    };
    const scroll = () => {
      if (!frame) frame = window.requestAnimationFrame(update);
    };
    scroller.addEventListener("scroll", scroll, { passive: true });
    return () => {
      scroller.removeEventListener("scroll", scroll);
      if (frame) window.cancelAnimationFrame(frame);
    };
  }, [rendered]);

  useEffect(() => {
    const target = host.current;
    const active = session.current;
    if (!target || !active) return;
    const resize = () => {
      const page = active.body.querySelector<HTMLElement>("section.sixa-word");
      const natural = page?.offsetWidth || 794;
      const widthScale = Math.max(0.2, (target.clientWidth - 24) / natural);
      const heightScale =
        (window.innerHeight - 290) / (page?.offsetHeight || 1123);
      const scale =
        zoom *
        (fitMode === "page" ? Math.min(widthScale, heightScale) : widthScale);
      active.body.style.zoom = String(Math.max(0.2, Math.min(scale, 3)));
    };
    resize();
    const observer =
      typeof ResizeObserver === "undefined"
        ? undefined
        : new ResizeObserver(resize);
    observer?.observe(target);
    return () => observer?.disconnect();
  }, [rendered, zoom, fitMode]);

  const fullContent = !!layoutReason;
  const fallbackAnchors = fullContent ? (preview?.anchors ?? []) : otherAnchors;
  return (
    <div
      className="sixa-docx-preview"
      data-preview-kind={
        documentResult
          ? "generated-docx"
          : result
            ? "draft-docx"
            : "source-docx"
      }
    >
      <div className="sixa-docx-note">
        <span>
          {documentResult
            ? "已生成文件 · DOCX 排版预览"
            : result
              ? "复核效果 · 尚未生成文件"
              : "DOCX 排版预览"}
          ，排版可能与 Word 不同。
        </span>
        {!documentResult && result && (
          <span>未可靠定位的内容保留原文，请在“其他内容复核”中查看。</span>
        )}
      </div>
      {busy && (
        <div className="sixa-docx-state" role="status">
          <LoaderCircle className="spin" size={18} />
          正在加载文档排版
        </div>
      )}
      {!busy && error && (
        <div className="sixa-docx-state" role="alert">
          <span>{error}</span>
          <button
            type="button"
            className="secondary"
            onClick={() => setRetry((value) => value + 1)}
          >
            {documentResult ? "重试结果预览" : "重试文档预览"}
          </button>
        </div>
      )}
      <div
        className="sixa-docx-host"
        ref={host}
        hidden={busy || !!error || fullContent}
        aria-label={documentResult ? "已生成 DOCX 预览" : "DOCX 排版内容"}
      />
      {!busy && !error && layoutReason && (
        <div className="sixa-docx-fallback-note">
          <span>{layoutReason}</span>
          <button
            type="button"
            className="secondary"
            onClick={() => setRetry((value) => value + 1)}
          >
            重试排版
          </button>
        </div>
      )}
      {!busy &&
        !error &&
        preview &&
        fallbackAnchors.length > 0 &&
        (fullContent ? (
          <VirtualContent
            anchors={fallbackAnchors}
            entities={documentResult ? [] : task.entities}
            draft={result && !documentResult}
            focusedId={focusedId}
            onFocus={props.onFocusEntity}
          />
        ) : (
          <details
            className="sixa-docx-other"
            open={otherOpen}
            onToggle={(event) => setOtherOpen(event.currentTarget.open)}
          >
            <summary>其他内容复核 · {fallbackAnchors.length} 个片段</summary>
            <p>批注、文本框或未可靠映射的片段在此按文档顺序展示。</p>
            {otherOpen && (
              <VirtualContent
                anchors={fallbackAnchors}
                entities={documentResult ? [] : task.entities}
                draft={result && !documentResult}
                focusedId={focusedId}
                onFocus={props.onFocusEntity}
              />
            )}
          </details>
        ))}
      {!busy && !error && preview && preview.images.length > 0 && (
        <div className="sixa-docx-media">
          <span>嵌入图片：修改会应用到同一图片的全部引用。</span>
          <div>
            {preview.images.map((image) => (
              <button
                type="button"
                className="secondary"
                key={image.index}
                onClick={() => props.onOpenImage(image.index)}
              >
                {image.name || `图片 ${image.index + 1}`} ·{" "}
                {image.occurrences.length} 处引用
              </button>
            ))}
          </div>
        </div>
      )}
      {!busy && !error && preview && preview.warnings.length > 0 && (
        <details className="sixa-docx-warnings">
          <summary>文档提示 · {preview.warnings.length}</summary>
          <ul>
            {preview.warnings.map((warning, index) => (
              <li key={index}>{warning}</li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

function overlaps(anchor: OfficeAnchor, entity: Entity) {
  return (
    anchor.display.start < entity.display.end &&
    anchor.display.end > entity.display.start
  );
}
function scrollContainer(host: HTMLElement): HTMLElement {
  let node: HTMLElement | null = host.parentElement;
  while (node) {
    if (
      /(auto|scroll)/.test(getComputedStyle(node).overflowY) &&
      node.scrollHeight > node.clientHeight + 1
    )
      return node;
    node = node.parentElement;
  }
  return (
    host.closest<HTMLElement>(".preview-pane,.canvas-scroll") ??
    document.documentElement
  );
}
function scrollMarker(host: HTMLElement, marker: HTMLElement, offset: number) {
  const scroller = scrollContainer(host);
  const difference =
    marker.getBoundingClientRect().top +
    offset -
    scroller.getBoundingClientRect().top -
    8;
  if (Math.abs(difference) > 1) scroller.scrollTop += difference;
}
function anchorHeight(marker: HTMLElement, bindings: AnchorMap): number {
  const page = marker.closest("section.sixa-word");
  const markers = [
    ...(page ?? marker.parentElement!).querySelectorAll<HTMLElement>("[id]"),
  ].filter((item) => bindings.has(item.id));
  const next = markers[markers.indexOf(marker) + 1];
  return Math.max(
    1,
    (next?.getBoundingClientRect().top ??
      page?.getBoundingClientRect().bottom ??
      marker.getBoundingClientRect().bottom) -
      marker.getBoundingClientRect().top,
  );
}

function VirtualContent({
  anchors,
  entities,
  draft,
  focusedId,
  onFocus,
}: {
  anchors: OfficeAnchor[];
  entities: Entity[];
  draft: boolean;
  focusedId?: string | null;
  onFocus: (id: string) => void;
}) {
  const chunks = useMemo(() => contentChunks(anchors), [anchors]);
  const viewport = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [width, setWidth] = useState(650);
  const [heights, setHeights] = useState<Record<number, number>>({});
  const offsets = useMemo(() => {
    const values = [0];
    const columns = Math.max(12, Math.floor((width - 38) / 14));
    chunks.forEach((chunk, index) =>
      values.push(
        values[index] +
          (heights[index] ??
            48 +
              chunk.text
                .split("\n")
                .reduce(
                  (count, line) =>
                    count + Math.max(1, Math.ceil(line.length / columns)),
                  0,
                ) *
                22),
      ),
    );
    return values;
  }, [chunks, width, heights]);
  useEffect(() => {
    setHeights({});
  }, [chunks, width, draft, entities]);
  useEffect(() => {
    const element = viewport.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) =>
      setWidth(entry.contentRect.width),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const entity = entities.find((entity) => entity.id === focusedId);
    const index = entity
      ? chunks.findIndex((chunk) => overlaps(chunk, entity))
      : -1;
    if (index >= 0 && viewport.current) {
      viewport.current.scrollTop = offsets[index];
      setScrollTop(offsets[index]);
    }
  }, [focusedId, chunks]);
  const first = Math.max(
    0,
    offsets.findIndex(
      (_, index) => index < chunks.length && offsets[index + 1] > scrollTop,
    ) - 2,
  );
  const end = offsets.findIndex((offset) => offset > scrollTop + 700);
  const last =
    end === -1
      ? chunks.length
      : Math.min(chunks.length, Math.max(first + 1, end + 2));
  return (
    <div
      className="sixa-docx-content"
      ref={viewport}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
      aria-label="完整分块内容"
    >
      <div style={{ height: offsets.at(-1), position: "relative" }}>
        {chunks.slice(first, last).map((chunk, relative) => (
          <ContentRow
            key={`${chunk.id}:${chunk.display.start}`}
            anchor={chunk}
            top={offsets[first + relative]}
            entities={entities}
            draft={draft}
            focusedId={focusedId}
            onFocus={onFocus}
            onHeight={(height) =>
              setHeights((values) =>
                values[first + relative] === height
                  ? values
                  : { ...values, [first + relative]: height },
              )
            }
          />
        ))}
      </div>
    </div>
  );
}

function ContentRow({
  anchor,
  top,
  entities,
  draft,
  focusedId,
  onFocus,
  onHeight,
}: {
  anchor: OfficeAnchor;
  top: number;
  entities: Entity[];
  draft: boolean;
  focusedId?: string | null;
  onFocus: (id: string) => void;
  onHeight: (height: number) => void;
}) {
  const row = useRef<HTMLDivElement>(null);
  const heightCallback = useRef(onHeight);
  heightCallback.current = onHeight;
  useEffect(() => {
    if (!row.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (row.current) heightCallback.current(row.current.offsetHeight);
    });
    observer.observe(row.current);
    return () => observer.disconnect();
  }, []);
  return (
    <div
      ref={row}
      className="sixa-docx-content-row"
      style={{ position: "absolute", top, left: 0, right: 0 } as CSSProperties}
    >
      <span className="sixa-docx-content-label">{anchor.label}</span>
      <div>
        {textParts(anchor.text, anchor.display.start, entities, draft).map(
          (part, index) =>
            part.entity ? (
              <mark
                key={index}
                role="button"
                tabIndex={0}
                className={`${part.entity.selected ? "is-selected" : ""} ${part.entity.id === focusedId ? "is-focused" : ""}`}
                onClick={() => onFocus(part.entity!.id)}
                onKeyDown={(event) => {
                  if (["Enter", " "].includes(event.key)) {
                    event.preventDefault();
                    onFocus(part.entity!.id);
                  }
                }}
              >
                {part.text}
              </mark>
            ) : (
              part.text
            ),
        )}
      </div>
    </div>
  );
}
