from __future__ import annotations

import re
from io import BytesIO
from pathlib import Path

from fastapi.testclient import TestClient
from PIL import Image

from app.main import app
from app.policies import DEFAULT_POLICIES, policy_for


client = TestClient(app)


def test_builtin_policies_cover_common_entities_with_chinese_replacements():
    required = {
        "PERSON": "某人",
        "ORGANIZATION": "某机构",
        "LOCATION": "某地点",
        "ADDRESS": "某地址",
        "EMAIL": "***@***",
        "IP_ADDRESS": "0.0.0.0",
    }
    for entity_type, replacement in required.items():
        policy = policy_for(entity_type)
        assert policy.get("text_action")
        assert policy.get("replacement") == replacement
        assert policy.get("image_action") in {"blur", "pixelate", "solid", "text", "keep"}
    assert "DEFAULT" in DEFAULT_POLICIES


def test_image_default_mask_blurs_and_draws_replacement():
    source = Image.new("RGB", (180, 80), "white")
    # High-frequency region makes blur observable in the output.
    for x in range(40, 140, 2):
        for y in range(20, 60, 2):
            source.putpixel((x, y), (0, 0, 0))
    data = BytesIO()
    source.save(data, format="PNG")
    files = {"file": ("sample.png", data.getvalue(), "image/png")}
    analyzed = client.post("/api/image/analyze", files=files)
    assert analyzed.status_code == 200, analyzed.text
    payload = analyzed.json()
    # Explicit region keeps this test independent of OCR availability.
    boxes = [{"x": 40 / 180, "y": 20 / 80, "width": 100 / 180, "height": 40 / 80,
              "text": "张三", "type": "PERSON", "selected": True}]
    masked = client.post("/api/image/mask", json={"analysis_id": payload["analysis_id"], "boxes": boxes})
    assert masked.status_code == 200, masked.text
    result = masked.json()
    output = client.get(result["artifact"])
    assert output.status_code == 200
    rendered = Image.open(BytesIO(output.content)).convert("RGB")
    assert rendered.size == source.size
    # Masking should alter pixels in the selected region.
    assert rendered.getpixel((90, 40)) != source.getpixel((90, 40))


def test_file_mask_does_not_require_review_flag():
    text = "联系人张三，电话13800138000"
    analyzed = client.post("/api/files/analyze", files={"file": ("a.txt", text.encode(), "text/plain")})
    assert analyzed.status_code == 200, analyzed.text
    payload = analyzed.json()
    masked = client.post("/api/files/mask", json={
        "analysis_id": payload["analysis_id"],
        "filename": "a.txt",
        "text": text,
        "entities": payload.get("entities", []),
    })
    assert masked.status_code == 200, masked.text


def test_frontend_is_chinese_and_has_no_global_review_confirmation():
    html = Path("app/static/index.html").read_text(encoding="utf-8")
    assert "lang=\"zh-CN\"" in html
    assert "我已复核全部实体和页面区域" not in html
    assert "我已复核上方实体" not in html
    # User-facing strategy labels should be Chinese rather than raw enum values.
    for label in ("模糊", "像素化", "纯色遮盖", "执行脱敏"):
        assert label in html
    assert not re.search(r">\s*(awaiting_review|completed|PHONE_NUMBER)\s*<", html)
