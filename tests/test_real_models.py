"""Opt-in end-to-end checks that load the real local OCR and RaNER models."""

from __future__ import annotations

import io
import os
from pathlib import Path

import fitz
import pytest
from fastapi.testclient import TestClient
from PIL import Image, ImageChops, ImageDraw, ImageFont

from app import main


pytestmark = pytest.mark.skipif(
    os.getenv("RUN_REAL_MODELS") != "1",
    reason="set RUN_REAL_MODELS=1 to load the local PaddleOCR and RaNER models",
)


def _phone_image(path: Path) -> Image.Image:
    image = Image.new("RGB", (1000, 220), "white")
    font_path = Path("C:/Windows/Fonts/arial.ttf")
    font = ImageFont.truetype(str(font_path), 82) if font_path.is_file() else ImageFont.load_default()
    ImageDraw.Draw(image).text((35, 55), "13800138000", font=font, fill="black")
    image.save(path, "PNG")
    return image


def _scan_pdf(image_path: Path, output_path: Path) -> None:
    document = fitz.open()
    page = document.new_page(width=1000, height=220)
    page.insert_image(page.rect, filename=str(image_path))
    document.save(output_path)
    document.close()


def _render_pdf(data: bytes) -> Image.Image:
    document = fitz.open(stream=data, filetype="pdf")
    pixmap = document[0].get_pixmap(alpha=False, colorspace=fitz.csRGB)
    image = Image.frombytes("RGB", (pixmap.width, pixmap.height), pixmap.samples)
    document.close()
    return image


def test_real_paddleocr_image_and_scanned_pdf_workflow(tmp_path: Path) -> None:
    image_path = tmp_path / "真实手机号.png"
    original_image = _phone_image(image_path)
    pdf_path = tmp_path / "扫描件.pdf"
    _scan_pdf(image_path, pdf_path)

    main.ocr_service.load()
    assert main.ocr_service.available, main.ocr_service.error
    direct_boxes = main.ocr_service.analyze(str(image_path))
    assert any("13800138000" in box["text"] for box in direct_boxes), direct_boxes

    with TestClient(main.app) as client:
        image_analysis = client.post(
            "/api/files/analyze",
            files={"file": (image_path.name, image_path.read_bytes(), "image/png")},
        )
        assert image_analysis.status_code == 200, image_analysis.text
        image_payload = image_analysis.json()
        assert any(entity["type"] == "PHONE" for entity in image_payload["entities"])
        image_mask = client.post(
            "/api/files/mask",
            json={
                "analysis_id": image_payload["analysis_id"],
                "filename": image_payload["filename"],
                "text": image_payload["text"],
                "entities": image_payload["entities"],
                "boxes": image_payload["boxes"],
                "reviewed": True,
            },
        )
        assert image_mask.status_code == 200, image_mask.text
        masked_image_response = client.get(image_mask.json()["artifact"])
        assert masked_image_response.status_code == 200
        masked_image = Image.open(io.BytesIO(masked_image_response.content)).convert("RGB")
        assert ImageChops.difference(original_image, masked_image).getbbox() is not None

        pdf_analysis = client.post(
            "/api/files/analyze",
            files={"file": (pdf_path.name, pdf_path.read_bytes(), "application/pdf")},
        )
        assert pdf_analysis.status_code == 200, pdf_analysis.text
        pdf_payload = pdf_analysis.json()
        assert any(item["code"] == "PDF_OCR_USED" for item in pdf_payload["warnings"])
        assert any(entity["type"] == "PHONE" for entity in pdf_payload["entities"])
        pdf_mask = client.post(
            "/api/files/mask",
            json={
                "analysis_id": pdf_payload["analysis_id"],
                "filename": pdf_payload["filename"],
                "text": pdf_payload["text"],
                "entities": pdf_payload["entities"],
                "boxes": pdf_payload["boxes"],
                "reviewed": True,
            },
        )
        assert pdf_mask.status_code == 200, pdf_mask.text
        masked_pdf_response = client.get(pdf_mask.json()["artifact"])
        assert masked_pdf_response.status_code == 200
        masked_document = fitz.open(stream=masked_pdf_response.content, filetype="pdf")
        extracted = "\n".join(page.get_text() for page in masked_document)
        masked_document.close()
        assert "13800138000" not in extracted
        assert "138****8000" in extracted
        assert ImageChops.difference(
            _render_pdf(pdf_path.read_bytes()),
            _render_pdf(masked_pdf_response.content),
        ).getbbox() is not None


def test_real_raner_chinese_entities() -> None:
    main.ner_service.load(download=False, device="cpu")
    assert main.ner_service.available, main.ner_service.error
    entities = main.ner_service.analyze("张三在北京大学工作。")
    assert any(item["type"] == "PERSON" and item["text"] == "张三" for item in entities)
    assert any(
        item["type"] in {"ORGANIZATION", "LOCATION", "GPE"}
        and item["text"] == "北京大学"
        for item in entities
    )
