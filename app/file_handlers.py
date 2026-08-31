from __future__ import annotations

import io
import re
import zipfile
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw, ImageFilter

from .policies import apply_entities, policy_for

SUPPORTED = {"txt", "md", "docx", "xlsx", "xlsm", "pdf", "png", "jpg", "jpeg", "bmp", "tif", "tiff"}


def extension(filename: str) -> str:
    return Path(filename or "").suffix.lower().lstrip(".")


def _text_from_docx(data: bytes) -> str:
    from docx import Document

    document = Document(io.BytesIO(data))
    parts = [p.text for p in document.paragraphs]
    for table in document.tables:
        parts.extend(cell.text for row in table.rows for cell in row.cells)
    for section in document.sections:
        parts.extend(p.text for p in section.header.paragraphs)
        parts.extend(p.text for p in section.footer.paragraphs)
    return "\n".join(parts)


def _text_from_xlsx(data: bytes, keep_vba: bool = False) -> str:
    from openpyxl import load_workbook

    workbook = load_workbook(io.BytesIO(data), read_only=False, data_only=False, keep_vba=keep_vba)
    parts: list[str] = []
    for sheet in workbook.worksheets:
        parts.append(f"[{sheet.title}]")
        for row in sheet.iter_rows():
            for cell in row:
                if cell.value is not None:
                    parts.append(str(cell.value))
                if cell.comment:
                    parts.append(cell.comment.text)
                if cell.hyperlink and cell.hyperlink.target:
                    parts.append(cell.hyperlink.target)
    for name, defined in workbook.defined_names.items():
        parts.extend([name, str(defined)])
    return "\n".join(parts)


def extract_text(data: bytes, filename: str) -> str:
    ext = extension(filename)
    if ext in {"txt", "md"}:
        for encoding in ("utf-8-sig", "gb18030"):
            try:
                return data.decode(encoding)
            except UnicodeDecodeError:
                continue
        return data.decode("utf-8", "replace")
    if ext == "docx":
        return _text_from_docx(data)
    if ext in {"xlsx", "xlsm"}:
        return _text_from_xlsx(data, keep_vba=ext == "xlsm")
    if ext == "pdf":
        import fitz

        document = fitz.open(stream=data, filetype="pdf")
        return "\n".join(page.get_text() for page in document)
    return ""


def _replace_docx_text(data: bytes, entities: list[dict], policies: dict | None) -> bytes:
    from docx import Document

    document = Document(io.BytesIO(data))
    containers = list(document.paragraphs)
    for table in document.tables:
        containers.extend(cell.paragraphs for row in table.rows for cell in row.cells)
    for section in document.sections:
        containers.extend([*section.header.paragraphs, *section.footer.paragraphs])
    for paragraph in containers:
        if isinstance(paragraph, list):
            for item in paragraph:
                _replace_docx_paragraph(item, entities, policies)
        else:
            _replace_docx_paragraph(paragraph, entities, policies)
    output = io.BytesIO()
    document.save(output)
    return output.getvalue()


def _replace_docx_paragraph(paragraph: Any, entities: list[dict], policies: dict | None) -> None:
    if not paragraph.runs:
        return
    original = paragraph.text
    spans = []
    for entity in entities:
        target = str(entity.get("text", ""))
        if not target:
            continue
        spans.extend({**entity, "start": match.start(), "end": match.end()} for match in re.finditer(re.escape(target), original))
    if not spans:
        return
    masked, _ = apply_entities(original, spans, policies)
    # Rebuild only text runs while cloning each run's formatting. This keeps
    # unaffected prefix/suffix runs intact even when an entity crosses runs.
    from copy import deepcopy
    chars = []
    for run_index, run in enumerate(paragraph.runs):
        for char in run.text or "":
            chars.append((char, run_index))
    if not chars:
        return
    replacement_by_start = {int(s["start"]): (int(s["end"]), masked[int(s["start"]):int(s["start"])] if False else None) for s in spans}
    chunks = []
    cursor = 0
    for span in sorted(spans, key=lambda s: (int(s["start"]), int(s["end"])), reverse=False):
        start, end = int(span["start"]), int(span["end"])
        if start < cursor:
            continue
        chunks.append(("text", original[cursor:start], cursor))
        replacement, _ = apply_entities(original[start:end], [{**span, "start": 0, "end": end - start}], policies)
        chunks.append(("replacement", replacement, start))
        cursor = end
    chunks.append(("text", original[cursor:], cursor))
    templates = [deepcopy(run._r.rPr) if run._r.rPr is not None else None for run in paragraph.runs]
    for run in paragraph.runs:
        run.text = ""
    cursor = 0
    for kind, value, source_start in chunks:
        if not value:
            continue
        run_index = next((idx for idx, (start, end) in enumerate(_run_ranges(original, paragraph)) if start <= source_start < end), 0)
        new_run = paragraph.add_run(value)
        if templates[run_index] is not None:
            new_run._r.get_or_add_rPr()._element = deepcopy(templates[run_index])
        cursor += len(value)


def _run_ranges(original: str, paragraph: Any) -> list[tuple[int, int]]:
    ranges = []
    cursor = 0
    for run in paragraph.runs:
        end = cursor + len(run.text or "")
        ranges.append((cursor, end))
        cursor = end
    return ranges


def _replace_xlsx(data: bytes, entities: list[dict], policies: dict | None, filename: str) -> bytes:
    from openpyxl import load_workbook

    workbook = load_workbook(io.BytesIO(data), read_only=False, data_only=False, keep_vba=filename.lower().endswith(".xlsm"))
    mapping: dict[tuple[str, str], str] = {}
    for sheet in workbook.worksheets:
        for row in sheet.iter_rows():
            for cell in row:
                values: list[tuple[str, str]] = []
                if isinstance(cell.value, str):
                    values.append(("value", cell.value))
                if cell.comment:
                    values.append(("comment", cell.comment.text))
                if cell.hyperlink and cell.hyperlink.target:
                    values.append(("url", cell.hyperlink.target))
                for kind, value in values:
                    found = []
                    for entity in entities:
                        target = str(entity.get("text", ""))
                        found.extend({**entity, "start": match.start(), "end": match.end()} for match in re.finditer(re.escape(target), value) if target)
                    if not found:
                        continue
                    masked, mapping = apply_entities(value, found, policies, mapping)
                    if kind == "value":
                        cell.value = masked
                    elif kind == "comment":
                        cell.comment.text = masked
                    else:
                        cell.hyperlink.target = masked
    output = io.BytesIO()
    workbook.save(output)
    return output.getvalue()


def _redact_pdf(data: bytes, entities: list[dict], policies: dict | None) -> bytes:
    import fitz

    document = fitz.open(stream=data, filetype="pdf")
    page_texts = [page.get_text() for page in document]
    offsets = []
    cursor = 0
    for value in page_texts:
        offsets.append((cursor, cursor + len(value)))
        cursor += len(value) + 1
        for page_number, pdf_page in enumerate(document):
            words = pdf_page.get_text("words")
            for entity in entities:
                target_page = int(entity.get("page") or next((i + 1 for i, (start, end) in enumerate(offsets) if start <= int(entity.get("start", 0)) < end), 1))
                if target_page != page_number + 1:
                    continue
                target = str(entity.get("text", ""))
                rectangles = [fitz.Rect(w[0], w[1], w[2], w[3]) for w in words if target and target in w[4]]
                if not rectangles and target:
                    rectangles = [fitz.Rect(w[0], w[1], w[2], w[3]) for w in words if w[4] and w[4] in target]
                action = policy_for(entity.get("type", "DEFAULT"), policies).get("text_action", "replace")
                if action == "keep":
                    continue
                replacement = "***" if action in {"mask", "token"} else str(policy_for(entity.get("type", "DEFAULT"), policies).get("replacement") or "已脱敏")
                for rect in rectangles:
                    pdf_page.add_redact_annot(rect, text=replacement, fill=(0, 0, 0), text_color=(1, 1, 1))
            pdf_page.apply_redactions()
    output = io.BytesIO()
    document.set_metadata({})
    document.save(output)
    return output.getvalue()


def _redact_image(data: bytes, entities: list[dict], filename: str, policies: dict | None = None, boxes: list[dict] | None = None) -> bytes:
    image = Image.open(io.BytesIO(data)).convert("RGBA")
    all_boxes = [*entities, *(boxes or [])]
    for entity in all_boxes:
        bbox = entity.get("bbox") or entity
        if not bbox:
            continue
        x, y, width, height = float(bbox.get("x", 0)), float(bbox.get("y", 0)), float(bbox.get("width", 0)), float(bbox.get("height", 0))
        if not all(map(lambda value: value == value, (x, y, width, height))) or x < 0 or y < 0 or width <= 0 or height <= 0 or x + width > 1 or y + height > 1:
            raise ValueError("图片框坐标必须位于 0 到 1 范围内")
        left, top = max(0, int(x * image.width)), max(0, int(y * image.height))
        right, bottom = min(image.width, int((x + width) * image.width)), min(image.height, int((y + height) * image.height))
        if right <= left or bottom <= top:
            continue
        action = policy_for(entity.get("type", "DEFAULT"), policies).get("image_action", "solid")
        if action == "keep":
            continue
        region = image.crop((left, top, right, bottom))
        if action == "blur":
            region = region.filter(ImageFilter.GaussianBlur(14))
            image.alpha_composite(region, (left, top))
        elif action == "pixelate":
            small = region.resize((max(1, region.width // 12), max(1, region.height // 12)))
            image.alpha_composite(small.resize(region.size, Image.Resampling.NEAREST), (left, top))
        elif action == "text":
            draw = ImageDraw.Draw(image)
            draw.rectangle((left, top, right, bottom), fill=(255, 255, 255, 255))
            draw.text((left + 2, top + 2), str(policy_for(entity.get("type", "DEFAULT"), policies).get("replacement") or "已脱敏"), fill=(0, 0, 0, 255))
        else:
            color = policy_for(entity.get("type", "DEFAULT"), policies).get("color", "#000000").lstrip("#")
            color = (color + "000000")[:6]
            fill = tuple(int(color[i:i + 2], 16) for i in (0, 2, 4)) + (255,)
            image.paste(fill, (left, top, right, bottom))
    output = io.BytesIO()
    ext = extension(filename)
    save_format = {"jpg": "JPEG", "jpeg": "JPEG", "bmp": "BMP", "tif": "TIFF", "tiff": "TIFF", "png": "PNG"}.get(ext, "PNG")
    image.convert("RGB" if save_format in {"JPEG", "BMP"} else "RGBA").save(output, format=save_format, exif=b"" if save_format in {"JPEG", "TIFF"} else None)
    return output.getvalue()


def mask_file(data: bytes, filename: str, text: str, entities: list[dict], policies: dict | None = None, boxes: list[dict] | None = None) -> bytes:
    ext = extension(filename)
    if ext in {"txt", "md"}:
        return apply_entities(text, entities, policies)[0].encode("utf-8")
    if ext == "docx":
        return _replace_docx_text(data, entities, policies)
    if ext in {"xlsx", "xlsm"}:
        return _replace_xlsx(data, entities, policies, filename)
    if ext == "pdf":
        return _redact_pdf(data, entities, policies)
    if ext in {"png", "jpg", "jpeg", "bmp", "tif", "tiff"}:
        return _redact_image(data, entities, filename, policies, boxes)
    raise ValueError(f"不支持的文件格式: .{ext}")


def content_manifest(filename: str, text: str, entities: list[dict]) -> dict:
    return {"filename": filename, "text_length": len(text), "entity_count": len(entities), "entities": entities}
