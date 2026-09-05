"""Optional visual/privacy regression tests for real resume PDFs.

The customer resume directory is deliberately opt-in: CI and fresh checkouts
do not need access to private documents.  When the directory exists, these
tests copy no files and only operate on in-memory bytes.
"""

from __future__ import annotations

import io
import os
import re
from pathlib import Path

import fitz
import pytest
from PIL import Image, ImageChops

from app.file_handlers import _redact_pdf


DEFAULT_RESUME_DIR = Path(
    r"E:\WXWORK\WXWork\1688857401657171\Cache\File\2026-03"
)
SENSITIVE = (
    ("PHONE", re.compile(r"(?<!\d)(?:1[3-9]\d{9}|0\d{2,3}-?\d{7,8})(?!\d)")),
    ("EMAIL", re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")),
    ("ID_CARD", re.compile(r"(?<![0-9A-Za-z])\d{17}[\dXx](?![0-9A-Za-z])")),
)


def _resume_files() -> list[Path]:
    root = Path(os.environ.get("DESENSITIZATION_REAL_PDF_DIR", str(DEFAULT_RESUME_DIR)))
    if not root.is_dir():
        return []
    return sorted(root.glob("*.pdf"))[:8]


def _entities(document: fitz.Document) -> tuple[list[dict], set[str]]:
    """Build deterministic entities from high-confidence values in page text."""
    entities: list[dict] = []
    values: set[str] = set()
    offset = 0
    for page_number, page in enumerate(document, 1):
        text = page.get_text(sort=False)
        for kind, pattern in SENSITIVE:
            for match in pattern.finditer(text):
                value = match.group()
                rects = page.search_for(value)
                if not rects:
                    continue
                rect = rects[0]
                entities.append(
                    {
                        "type": kind,
                        "text": value,
                        "start": offset + match.start(),
                        "end": offset + match.end(),
                        "page": page_number,
                        "selected": True,
                        "bbox": {
                            "x": rect.x0,
                            "y": rect.y0,
                            "width": rect.width,
                            "height": rect.height,
                        },
                    }
                )
                values.add(value)
        offset += len(text) + 1
    return entities, values


def _render(document: fitz.Document, page_number: int) -> Image.Image:
    pixmap = document[page_number].get_pixmap(matrix=fitz.Matrix(1, 1), alpha=False)
    return Image.open(io.BytesIO(pixmap.tobytes("png"))).convert("RGB")


@pytest.mark.skipif(not _resume_files(), reason="customer resume PDFs are not available")
def test_real_resumes_mask_locally_without_destroying_layout() -> None:
    """Sensitive regions change, while the rest of each page remains stable."""
    checked = 0
    for path in _resume_files():
        source_bytes = path.read_bytes()
        source = fitz.open(stream=source_bytes, filetype="pdf")
        entities, sensitive_values = _entities(source)
        if not entities:
            source.close()
            continue
        masked_bytes = _redact_pdf(source_bytes, entities, None)
        masked = fitz.open(stream=masked_bytes, filetype="pdf")
        assert masked.page_count == source.page_count, path.name
        assert all(value not in "\n".join(page.get_text() for page in masked) for value in sensitive_values), path.name
        assert b"???" not in masked_bytes, path.name

        # A correctly bounded local mask should not turn a resume into a page-
        # sized rectangle.  Keep a generous threshold for scanned/colorful PDFs.
        for page_number in range(source.page_count):
            before = _render(source, page_number)
            after = _render(masked, page_number)
            diff = ImageChops.difference(before, after)
            changed = sum(1 for pixel in diff.getdata() if max(pixel) > 8)
            total = before.width * before.height
            assert changed / total < 0.55, f"mask changed most of {path.name} page {page_number + 1}"
        source.close()
        masked.close()
        checked += 1
    assert checked, "resume PDFs were found but contained no high-confidence values"

