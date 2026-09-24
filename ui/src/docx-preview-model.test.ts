// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import {
  annotate,
  bindAnchors,
  bindImages,
  contentChunks,
  sanitizeCss,
  sanitizeRendered,
  type OfficePreview,
} from "./docx-preview-model";
import type { Entity } from "./api";

const person = (start: number, end: number): Entity => ({
  id: "person",
  entity_type: "PERSON",
  type_label: "姓名",
  score: 1,
  source: "test",
  selected: true,
  display: { start, end },
  text: "张三",
  replacement: null,
  effective_replacement: "某人",
});
function preview(texts: string[]): OfficePreview {
  let start = 0;
  return {
    revision: 1,
    layout_available: true,
    reason: null,
    warnings: [],
    images: [],
    anchors: texts.map((text, index) => {
      const anchor = {
        id: `sixa_text_${index}`,
        text,
        display: { start, end: start + text.length },
        label: "正文",
        available_in_layout: true,
      };
      start += text.length;
      return anchor;
    }),
  };
}
describe("DOCX bookmark mapping", () => {
  it("distinguishes repeated text by bookmark rather than matching the first occurrence", () => {
    const body = document.createElement("div");
    body.innerHTML =
      '<p><span id="sixa_text_0"></span><b>张三</b><span id="sixa_text_1"></span><i>张三</i></p>';
    const bindings = bindAnchors(body, preview(["张三", "张三"]));
    expect(annotate(bindings, [person(2, 4)], true)).toEqual([]);
    expect(body.querySelector("b")?.textContent).toBe("张三");
    expect(body.querySelector("i")?.textContent).toBe("某人");
    annotate(bindings, [person(2, 4)], false);
    expect(body.textContent).toBe("张三张三");
  });
  it("replaces an entity across styled runs once and restores the source on undo", () => {
    const body = document.createElement("div");
    body.innerHTML =
      '<p><span id="sixa_text_0"></span><b>张</b><span id="sixa_text_1"></span><i>三</i><span id="sixa_text_2"></span><u>公开</u></p>';
    const bindings = bindAnchors(body, preview(["张", "三", "公开"]));
    annotate(bindings, [person(0, 2)], true);
    expect(body.textContent).toBe("某人公开");
    expect(body.querySelector("b")?.textContent).toBe("某人");
    expect(body.querySelector("u")?.textContent).toBe("公开");
    annotate(bindings, [{ ...person(0, 2), selected: false }], true);
    expect(body.textContent).toBe("张三公开");
  });
  it("keeps unmatched and trailing source text intact", () => {
    const body = document.createElement("div");
    body.innerHTML =
      '<p><span id="sixa_text_0"></span><b>张三公开</b><span id="sixa_text_1"></span><i>不同文字</i><u>张三</u></p>';
    const bindings = bindAnchors(body, preview(["张三", "张三"]));
    expect(bindings.has("sixa_text_1")).toBe(false);
    annotate(bindings, [person(0, 2)], true);
    expect(body.querySelector("b")?.textContent).toBe("某人公开");
    expect(body.querySelector("i")?.textContent).toBe("不同文字");
    expect(body.querySelector("u")?.textContent).toBe("张三");
  });
  it("links every occurrence of a shared image to the same review index", () => {
    const body = document.createElement("div");
    body.innerHTML =
      '<p><span id="sixa_image_0_0"></span><img src="blob:first"><span id="sixa_image_0_1"></span><img src="blob:second"></p>';
    const meta = preview([]);
    meta.images = [
      {
        index: 0,
        name: "证件照",
        occurrences: ["sixa_image_0_0", "sixa_image_0_1"],
      },
    ];
    bindImages(body, meta);
    expect(
      [...body.querySelectorAll("img")].map(
        (image) => image.dataset.imageIndex,
      ),
    ).toEqual(["0", "0"]);
  });
  it("splits large fallback pieces without splitting UTF-16 surrogate pairs", () => {
    const chunks = contentChunks(preview(["张😀三公开"]).anchors, 2);
    expect(chunks.map((chunk) => chunk.text).join("")).toBe("张😀三公开");
    expect(chunks[1].text).toBe("😀");
    expect(chunks.at(-1)?.display.end).toBe(6);
  });
  it("keeps unchanged fragment nodes while an unrelated entity is edited", () => {
    const body = document.createElement("div");
    body.innerHTML =
      '<p><span id="sixa_text_0"></span><b>张三</b><span id="sixa_text_1"></span><i>张三</i></p>';
    const bindings = bindAnchors(body, preview(["张三", "张三"]));
    const second = { ...person(2, 4), id: "second" };
    annotate(bindings, [person(0, 2), second], false);
    const unchanged = body.querySelector("b mark");
    annotate(bindings, [person(0, 2), { ...second, selected: false }], false);
    expect(body.querySelector("b mark")).toBe(unchanged);
  });
});
describe("detached DOCX output sanitization", () => {
  it("removes navigation, active HTML and external resources while retaining blob images", () => {
    const body = document.createElement("div");
    const styles = document.createElement("div");
    body.innerHTML =
      '<a href="https://outside.test" ping="https://outside.test">正文</a><img src="https://outside.test/i.png" onerror="bad()"><img src="blob:owned"><iframe src="https://outside.test"></iframe><script>bad()</script>';
    styles.innerHTML =
      '<style>@import "https://outside.test/theme.css";</style><style>.safe { color: red; }</style>';
    expect([...sanitizeRendered(body, styles)]).toEqual(["blob:owned"]);
    expect(body.querySelector("a")?.hasAttribute("href")).toBe(false);
    expect(body.querySelector("a")?.hasAttribute("ping")).toBe(false);
    expect(body.querySelector("img")?.hasAttribute("src")).toBe(false);
    expect(body.querySelector("img")?.hasAttribute("onerror")).toBe(false);
    expect(body.querySelector("iframe,script")).toBeNull();
    expect(styles.textContent).toContain(".safe");
    expect(styles.textContent).not.toContain("outside.test");
  });
  it("rejects escaped CSS resource requests and relative URL resources", () => {
    expect(sanitizeCss('.x { background: u\\72l("//outside.test"); }')).toBe(
      "",
    );
    expect(
      sanitizeCss('.x { background: url("relative.png"); color: red; }'),
    ).toContain("background: none");
    expect(sanitizeCss('.x { background: url("blob:owned"); }')).toContain(
      "blob:owned",
    );
  });
});
