// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import JSZip from "jszip";
import { renderAsync } from "docx-preview";
import {
  bindAnchors,
  sanitizeRendered,
  type OfficePreview,
} from "./docx-preview-model";

describe("locked DOCX renderer compatibility", () => {
  it("preserves paragraph-level bookmarks and run formatting in a generated OOXML document", async () => {
    const zip = new JSZip();
    zip.file(
      "[Content_Types].xml",
      '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
    );
    zip.file(
      "_rels/.rels",
      '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
    );
    zip.file(
      "word/document.xml",
      '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:bookmarkStart w:id="100000" w:name="sixa_text_0"/><w:r><w:rPr><w:b/></w:rPr><w:t>张三</w:t></w:r><w:bookmarkEnd w:id="100000"/><w:bookmarkStart w:id="100001" w:name="sixa_text_1"/><w:r><w:rPr><w:i/></w:rPr><w:t>张三</w:t></w:r><w:bookmarkEnd w:id="100001"/></w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:body></w:document>',
    );
    const body = document.createElement("div"),
      styles = document.createElement("div");
    await renderAsync(
      await zip.generateAsync({ type: "uint8array" }),
      body,
      styles,
      {
        renderAltChunks: false,
        renderChanges: false,
        ignoreFonts: true,
        useBase64URL: false,
      },
    );
    sanitizeRendered(body, styles);
    const metadata: OfficePreview = {
      revision: 1,
      layout_available: true,
      reason: null,
      images: [],
      warnings: [],
      anchors: [0, 1].map((index) => ({
        id: `sixa_text_${index}`,
        text: "张三",
        display: { start: index * 2, end: index * 2 + 2 },
        label: "正文",
        available_in_layout: true,
      })),
    };
    const bindings = bindAnchors(body, metadata);
    expect(bindings.size).toBe(2);
    expect(
      bindings.get("sixa_text_0")![0].fragments[0].slot.parentElement!.style
        .fontWeight,
    ).toBe("bold");
    expect(
      bindings.get("sixa_text_1")![0].fragments[0].slot.parentElement!.style
        .fontStyle,
    ).toBe("italic");
  });
});
