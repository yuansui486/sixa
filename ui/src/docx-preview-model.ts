import type { Entity } from "./api";

export interface OfficeAnchor {
  id: string;
  text: string;
  display: { start: number; end: number };
  label: string;
  available_in_layout: boolean;
}
export interface OfficePreview {
  revision: number;
  layout_available: boolean;
  reason: string | null;
  anchors: OfficeAnchor[];
  images: { index: number; name: string; occurrences: string[] }[];
  warnings: string[];
}
export interface DocxScrollAnchor {
  anchorId: string;
  page: number;
  fraction?: number;
  offset?: number;
}
type Fragment = {
  slot: HTMLElement;
  text: string;
  suffix: string;
  start: number;
  end: number;
  signature?: string;
};
export interface AnchorBinding {
  anchor: OfficeAnchor;
  marker: HTMLElement;
  fragments: Fragment[];
}
export type AnchorMap = Map<string, AnchorBinding[]>;

/** Bookmark names come from the display copy, never from searching document text. */
export function bindAnchors(
  body: HTMLElement,
  preview: OfficePreview,
): AnchorMap {
  const anchors = new Map(
    preview.anchors
      .filter((anchor) => anchor.available_in_layout)
      .map((anchor) => [anchor.id, anchor]),
  );
  const markers = new Set([
    ...preview.anchors.map((anchor) => anchor.id),
    ...preview.images.flatMap((image) => image.occurrences),
  ]);
  const bindings: AnchorMap = new Map();
  const slots = new Map<Text, HTMLElement>();
  for (const marker of body.querySelectorAll<HTMLElement>("[id]")) {
    const anchor = anchors.get(marker.id);
    if (
      !anchor ||
      !anchor.text ||
      anchor.display.end - anchor.display.start !== anchor.text.length
    )
      continue;
    const walker = document.createTreeWalker(
      body,
      NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
    );
    walker.currentNode = marker;
    const pieces: { node: Text; text: string; offset: number }[] = [];
    let text = "";
    while (text.length < anchor.text.length) {
      const node = walker.nextNode();
      if (!node || (node instanceof HTMLElement && markers.has(node.id))) break;
      if (!(node instanceof Text)) continue;
      if (node.parentElement?.closest("style,script")) continue;
      // A run can contain more than one text node. Never consume beyond this anchor.
      const value = node.data.slice(0, anchor.text.length - text.length);
      pieces.push({ node, text: value, offset: text.length });
      text += value;
    }
    if (text !== anchor.text) continue;
    const fragments = pieces.map(({ node, text, offset }) => {
      let slot = slots.get(node);
      if (!slot) {
        slot = document.createElement("span");
        slot.className = "sixa-docx-text";
        slot.textContent = node.data;
        slots.set(node, slot);
        node.replaceWith(slot);
      }
      return {
        slot,
        text,
        suffix: node.data.slice(text.length),
        start: anchor.display.start + offset,
        end: anchor.display.start + offset + text.length,
      };
    });
    bindings.set(anchor.id, [
      ...(bindings.get(anchor.id) ?? []),
      { anchor, marker, fragments },
    ]);
  }
  return bindings;
}

export function entityCovered(entity: Entity, bindings: AnchorMap): boolean {
  return coveredBy(entity, coveredSpans(bindings));
}
function coveredSpans(bindings: AnchorMap) {
  return [...bindings.values()]
    .map((items) => items[0].anchor.display)
    .sort((a, b) => a.start - b.start);
}
function coveredBy(
  entity: Entity,
  spans: { start: number; end: number }[],
): boolean {
  let cursor = entity.display.start;
  let left = 0,
    right = spans.length;
  while (left < right) {
    const mid = (left + right) >>> 1;
    if (spans[mid].end <= cursor) left = mid + 1;
    else right = mid;
  }
  for (let index = left; index < spans.length; index++) {
    const span = spans[index];
    if (span.end <= cursor) continue;
    if (span.start > cursor) return false;
    cursor = span.end;
    if (cursor >= entity.display.end) return true;
  }
  return false;
}

export type TextPart = { text: string; entity?: Entity };
export function textParts(
  text: string,
  start: number,
  entities: Entity[],
  draft: boolean,
): TextPart[] {
  const parts: TextPart[] = [];
  const end = start + text.length;
  let cursor = start;
  for (const entity of entities
    .filter((item) => item.display.start < end && item.display.end > start)
    .sort((a, b) => a.display.start - b.display.start)) {
    const left = Math.max(cursor, entity.display.start);
    const right = Math.min(end, entity.display.end);
    if (left >= right) continue;
    if (left > cursor)
      parts.push({ text: text.slice(cursor - start, left - start) });
    const replacement = entity.replacement ?? entity.effective_replacement;
    parts.push({
      text:
        draft && entity.selected && replacement !== undefined
          ? entity.display.start >= start
            ? replacement
            : ""
          : text.slice(left - start, right - start),
      entity,
    });
    cursor = right;
  }
  if (cursor < end) parts.push({ text: text.slice(cursor - start) });
  return parts;
}

/** Rebuild only the marked text fragments, retaining every surrounding run/table style. */
export function annotate(
  bindings: AnchorMap,
  entities: Entity[],
  draft: boolean,
  focusedId?: string | null,
): string[] {
  const spans = coveredSpans(bindings);
  const incomplete = entities
    .filter((entity) => !coveredBy(entity, spans))
    .map((entity) => entity.id);
  const incompleteIds = new Set(incomplete);
  const affected = (
    draft
      ? entities.filter(
          (entity) => !entity.selected || !incompleteIds.has(entity.id),
        )
      : [...entities]
  ).sort((a, b) => a.display.start - b.display.start);
  for (const binding of [...bindings.values()].flat()) {
    for (const fragment of binding.fragments) {
      const { slot, text, suffix, start, end } = fragment;
      let left = 0,
        right = affected.length;
      while (left < right) {
        const mid = (left + right) >>> 1;
        if (affected[mid].display.end <= start) left = mid + 1;
        else right = mid;
      }
      let stop = left;
      while (stop < affected.length && affected[stop].display.start < end)
        stop++;
      const parts = textParts(text, start, affected.slice(left, stop), draft);
      const signature = JSON.stringify(
        parts.map((part) => [
          part.text,
          part.entity?.id,
          part.entity?.selected,
          part.entity?.type_label,
          part.entity?.text,
          part.entity?.id === focusedId,
        ]),
      );
      if (fragment.signature === signature) continue;
      fragment.signature = signature;
      slot.replaceChildren(
        ...parts.map((part) => {
          if (!part.entity) return document.createTextNode(part.text);
          const mark = document.createElement("mark");
          mark.dataset.entityId = part.entity.id;
          mark.className = `sixa-docx-entity${part.entity.selected ? " is-selected" : ""}${part.entity.id === focusedId ? " is-focused" : ""}`;
          mark.textContent = part.text;
          mark.tabIndex = 0;
          mark.setAttribute("role", "button");
          mark.title = `${part.entity.type_label}：${part.entity.text}`;
          return mark;
        }),
      );
      if (suffix) slot.append(document.createTextNode(suffix));
    }
  }
  return incomplete;
}

export function bindImages(body: HTMLElement, preview: OfficePreview) {
  const images = new Map(
    preview.images.flatMap((image) =>
      image.occurrences.map((id) => [id, image] as const),
    ),
  );
  const boundaries = new Set([
    ...preview.anchors.map((anchor) => anchor.id),
    ...images.keys(),
  ]);
  for (const marker of body.querySelectorAll<HTMLElement>("[id]")) {
    const image = images.get(marker.id);
    if (!image) continue;
    const walker = document.createTreeWalker(body, NodeFilter.SHOW_ELEMENT);
    walker.currentNode = marker;
    let node: Node | null;
    while ((node = walker.nextNode())) {
      const element = node as HTMLElement;
      if (boundaries.has(element.id)) break;
      if (!["img", "image"].includes(element.localName)) continue;
      element.dataset.imageIndex = String(image.index);
      element.classList.add("sixa-docx-image");
      element.setAttribute("tabindex", "0");
      element.setAttribute("role", "button");
      element.setAttribute(
        "aria-label",
        `复核${image.name}，共 ${image.occurrences.length} 处引用`,
      );
      element.setAttribute(
        "title",
        "点击复核图片；修改会应用到该图片的全部引用",
      );
      break;
    }
  }
}

export function sanitizeCss(css: string): string {
  const plain = css.replace(/\/\*[\s\S]*?\*\//g, "");
  // The renderer creates CSS, not arbitrary HTML. Reject escaped/resource-bearing
  // declarations before attaching its style nodes to a live shadow root.
  if (
    /\\|@import|@font-face|expression\s*\(|(?:-webkit-)?image-set\s*\(|(?:javascript|https?|file|data):|behavior\s*:|-moz-binding/i.test(
      plain,
    )
  )
    return "";
  return plain.replace(/url\s*\(([^)]*)\)/gi, (match, url: string) =>
    /^\s*["']?blob:[^\s"'()]+["']?\s*$/.test(url) ? match : "none",
  );
}

/** Run on detached renderer output, before mounting. No document-provided navigation. */
export function sanitizeRendered(
  body: HTMLElement,
  styles: HTMLElement,
): Set<string> {
  const urls = new Set<string>();
  for (const root of [body, styles]) {
    for (const element of root.querySelectorAll<HTMLElement>("*")) {
      for (const attribute of [...element.attributes]) {
        for (const url of attribute.value.match(/blob:[^\s'"()<>]+/g) ?? [])
          urls.add(url);
      }
      if (element.localName === "style") {
        for (const url of element.textContent?.match(/blob:[^\s'"()<>]+/g) ??
          [])
          urls.add(url);
        element.textContent = sanitizeCss(element.textContent ?? "");
      }
      if (
        [
          "script",
          "iframe",
          "object",
          "embed",
          "link",
          "meta",
          "base",
          "foreignobject",
          "video",
          "audio",
          "source",
          "form",
          "input",
        ].includes(element.localName.toLowerCase())
      ) {
        element.remove();
        continue;
      }
      for (const attribute of [...element.attributes]) {
        const name = attribute.name.toLowerCase();
        if (
          name.startsWith("on") ||
          [
            "srcset",
            "action",
            "formaction",
            "target",
            "download",
            "ping",
          ].includes(name)
        )
          element.removeAttribute(attribute.name);
        if (
          ["href", "xlink:href", "src"].includes(name) &&
          !(element.localName !== "a" && attribute.value.startsWith("blob:"))
        )
          element.removeAttribute(attribute.name);
      }
      if (element.hasAttribute("style"))
        element.setAttribute(
          "style",
          sanitizeCss(element.getAttribute("style") ?? ""),
        );
    }
  }
  return urls;
}

export function contentChunks(
  anchors: OfficeAnchor[],
  maxLength = 800,
): OfficeAnchor[] {
  maxLength = Math.max(2, maxLength);
  return anchors.flatMap((anchor) => {
    if (!anchor.text) return [anchor];
    const chunks: OfficeAnchor[] = [];
    let offset = 0;
    while (offset < anchor.text.length) {
      let end = Math.min(anchor.text.length, offset + maxLength);
      if (
        end < anchor.text.length &&
        /[\uD800-\uDBFF]/.test(anchor.text[end - 1])
      )
        end--;
      chunks.push({
        ...anchor,
        text: anchor.text.slice(offset, end),
        display: {
          start: anchor.display.start + offset,
          end: anchor.display.start + end,
        },
      });
      offset = end;
    }
    return chunks;
  });
}
