"""PDF masking visual and privacy regression tests.

The fixtures are generated in memory so they do not depend on downloaded OCR
models or customer documents.  These tests describe the expected PDF contract:
the default mask is soft, explicit actions remain available, and source values
are removed from the resulting document.
"""

from __future__ import annotations

import io

import fitz
import pytest
from PIL import Image, ImageChops

import app.file_handlers as file_handlers
from app.file_handlers import _redact_pdf
from app.policies import replacement_for


def _fixture_pdf(value: str = "ACME 13800138000") -> tuple[bytes, fitz.Rect]:
    document = fitz.open()
    page = document.new_page(width=420, height=140)
    # A colored background makes a hard black rectangle easy to distinguish
    # from the intended blurred patch.
    page.draw_rect(fitz.Rect(20, 35, 400, 100), color=(0.1, 0.55, 0.8), fill=(0.1, 0.55, 0.8))
    page.insert_text((35, 78), value, fontsize=26, color=(1, 1, 1))
    region = fitz.Rect(35, 50, 390, 88)
    raw = document.tobytes()
    document.close()
    return raw, region


def _render(data: bytes) -> Image.Image:
    document = fitz.open(stream=data, filetype="pdf")
    pixmap = document[0].get_pixmap(matrix=fitz.Matrix(1.5, 1.5), alpha=False)
    image = Image.open(io.BytesIO(pixmap.tobytes("png"))).convert("RGB")
    document.close()
    return image


def _entity(value: str, kind: str = "ORGANIZATION") -> dict:
    return {
        "type": kind,
        "text": value,
        "start": 0,
        "end": len(value),
        "selected": True,
        "bbox": {"x": 35, "y": 50, "width": 355, "height": 38},
    }


def test_pdf_default_mask_is_soft_and_not_black() -> None:
    raw, _ = _fixture_pdf("ACME")
    source = _render(raw)
    masked = _redact_pdf(raw, [_entity("ACME")], None)
    output = _render(masked)

    # The output must change in the selected area, but must not become a solid
    # black rectangle (the previous implementation's default behaviour).
    # ACME starts near x=35pt and is rendered at 1.5x scale.  Keep the crop
    # local to the entity; inspecting the whole line would dilute a hard mask
    # with unaffected background pixels.
    crop = output.crop((45, 65, 190, 130))
    colors = list(crop.getdata())
    assert ImageChops.difference(source, output).getbbox() is not None
    assert sum(sum(pixel) < 30 for pixel in colors) < len(colors) * 0.5


def test_pdf_removes_source_text_and_never_emits_question_mark_replacement() -> None:
    raw, _ = _fixture_pdf("ACME 13800138000")
    entity = _entity("ACME", "ORGANIZATION")
    masked = _redact_pdf(raw, [entity], None)
    document = fitz.open(stream=masked, filetype="pdf")
    extracted = "\n".join(page.get_text() for page in document)
    document.close()
    assert "ACME" not in extracted
    assert "???" not in extracted
    assert b"ACME" not in masked


def test_pdf_explicit_solid_action_remains_available() -> None:
    raw, _ = _fixture_pdf("ACME")
    masked = _redact_pdf(
        raw,
        [_entity("ACME")],
        {"ORGANIZATION": {"image_action": "solid", "color": "#000000", "show_replacement": False}},
    )
    output = _render(masked)
    # Explicit solid is intentionally distinct from the soft default.
    dark_pixels = sum(sum(pixel) < 30 for pixel in output.getdata())
    assert dark_pixels > 100


@pytest.mark.parametrize("action", ["blur", "pixelate", "text"])
def test_pdf_supported_visual_actions_change_region_and_remove_source(action: str) -> None:
    raw, _ = _fixture_pdf("ACME")
    policies = {
        "ORGANIZATION": {
            "image_action": action,
            "text_action": "replace",
            "replacement": "某公司",
            "show_replacement": True,
            "color": "#eeeeee",
        },
    }
    masked = _redact_pdf(raw, [_entity("ACME")], policies)
    assert ImageChops.difference(_render(raw), _render(masked)).getbbox() is not None
    result = fitz.open(stream=masked, filetype="pdf")
    assert "ACME" not in result[0].get_text()
    result.close()
    assert b"ACME" not in masked


def test_pdf_keep_action_preserves_page_pixels() -> None:
    raw, _ = _fixture_pdf("ACME")
    source = _render(raw)
    masked = _redact_pdf(raw, [_entity("ACME")], {"ORGANIZATION": {"text_action": "keep", "image_action": "keep"}})
    assert ImageChops.difference(source, _render(masked)).getbbox() is None


def test_organization_replacements_are_natural_chinese() -> None:
    policies = {
        "ORGANIZATION": {"text_action": "replace", "replacement": "某公司"},
    }
    assert replacement_for({"type": "ORGANIZATION", "text": "北京某科技有限公司"}, policies, {}) == "某公司"
    assert "?" not in replacement_for({"type": "ORGANIZATION", "text": "北京某科技有限公司"}, policies, {})


def test_pdf_ocr_parent_box_is_not_applied_after_linked_child(monkeypatch: pytest.MonkeyPatch) -> None:
    raw, _ = _fixture_pdf("ACME")
    calls: list[str] = []
    original = file_handlers._draw_mask_text

    def record(image: Image.Image, box: tuple[int, int, int, int], value: str) -> bool:
        calls.append(value)
        return original(image, box, value)

    monkeypatch.setattr(file_handlers, "_draw_mask_text", record)
    child = {**_entity("ACME"), "box_ids": ["line-1"]}
    parent = {
        "id": "line-1",
        "type": "DEFAULT",
        "text": "ACME",
        "page": 1,
        "selected": True,
        "bbox": {"x": 20, "y": 35, "width": 380, "height": 65},
    }
    _redact_pdf(raw, [child], None, [parent])
    assert len(calls) == 1
    assert calls[0] and "?" not in calls[0]


def test_pdf_redaction_removes_original_embedded_image_resource() -> None:
    source = Image.new("RGB", (240, 80), "white")
    for x in range(0, source.width, 4):
        for y in range(source.height):
            source.putpixel((x, y), (20, 90, 180))
    image_data = io.BytesIO()
    source.save(image_data, "PNG")
    source_bytes = image_data.getvalue()

    document = fitz.open()
    page = document.new_page(width=300, height=140)
    page.insert_image(fitz.Rect(30, 30, 270, 110), stream=source_bytes)
    raw = document.tobytes()
    document.close()
    box = {
        "id": "image-region",
        "type": "DEFAULT",
        "text": "",
        "page": 1,
        "selected": True,
        "bbox": {"x": 30, "y": 30, "width": 240, "height": 80},
    }
    masked = _redact_pdf(raw, [], None, [box])
    result = fitz.open(stream=masked, filetype="pdf")
    embedded = [result.extract_image(item[0])["image"] for item in result[0].get_images(full=True)]
    result.close()
    assert embedded
    assert source_bytes not in embedded
    assert ImageChops.difference(_render(raw), _render(masked)).getbbox() is not None
