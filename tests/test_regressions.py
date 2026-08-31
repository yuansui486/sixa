from io import BytesIO
from types import SimpleNamespace
import uuid

from fastapi.testclient import TestClient
from PIL import Image, ImageChops

from app.file_handlers import mask_file
from app.main import TASKS, _synchronize_review_selection, analyze, app
from app import policies


def _png(color="white"):
    image = Image.new("RGB", (100, 100), color)
    stream = BytesIO()
    image.save(stream, "PNG")
    return image, stream.getvalue()


def test_text_masking_executes_through_presidio_anonymizer(monkeypatch):
    calls = []

    class SpyAnonymizer:
        def anonymize(self, **kwargs):
            calls.append(kwargs)
            return SimpleNamespace(text="presidio-result")

    monkeypatch.setattr(policies, "_ANONYMIZER", SpyAnonymizer())
    masked, mapping = policies.apply_entities(
        "电话13800138000",
        [
            {
                "type": "PHONE",
                "text": "13800138000",
                "start": 2,
                "end": 13,
                "score": 0.95,
                "selected": True,
            }
        ],
    )
    assert masked == "presidio-result"
    assert mapping[("PHONE", "13800138000")] == "138****8000"
    assert calls[0]["analyzer_results"][0].entity_type == "PHONE"
    assert calls[0]["merge_entities_with_spaces"] is False


def test_deselected_image_entity_is_not_masked():
    original, data = _png()
    entity = {
        "type": "PHONE",
        "text": "13800138000",
        "start": 0,
        "end": 11,
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "selected": False,
    }

    masked = mask_file(data, "sample.png", "", [entity], {})
    result = Image.open(BytesIO(masked)).convert("RGB")
    assert ImageChops.difference(original, result).getbbox() is None


def test_selected_image_entity_is_masked():
    original, data = _png()
    entity = {
        "type": "PHONE",
        "text": "13800138000",
        "start": 0,
        "end": 11,
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "selected": True,
    }

    masked = mask_file(data, "sample.png", "", [entity], {})
    result = Image.open(BytesIO(masked)).convert("RGB")
    assert ImageChops.difference(original, result).getbbox() is not None


def test_ip_recognizer_rejects_adjacent_dotted_segments():
    entities = analyze("合法 192.168.1.1，非法 1.2.3.4.5")
    values = [item["text"] for item in entities if item["type"] == "IP_ADDRESS"]
    assert values == ["192.168.1.1"]


def test_image_entity_and_parent_box_are_processed_once_with_default_policy():
    original, data = _png()
    entity = {
        "id": "entity-1",
        "type": "PHONE",
        "text": "13800138000",
        "start": 0,
        "end": 11,
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "box_ids": ["box-1"],
        "selected": True,
    }
    box = {
        "id": "box-1",
        "type": "OCR",
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "selected": True,
    }
    masked = mask_file(
        data,
        "sample.png",
        "",
        [entity],
        {"DEFAULT": {"image_action": "keep"}},
        [box],
    )
    result = Image.open(BytesIO(masked)).convert("RGB")
    assert ImageChops.difference(original, result).getbbox() is None


def test_ocr_entity_and_parent_box_selection_is_synchronized():
    entity = {
        "id": "entity-1",
        "type": "PHONE",
        "text": "13800138000",
        "start": 0,
        "end": 11,
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "box_ids": ["box-1"],
        "selected": False,
    }
    box = {
        "id": "box-1",
        "type": "OCR",
        "source": "ocr",
        "entity_ids": ["entity-1"],
        "bbox": {"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5},
        "selected": True,
    }
    entities, boxes = _synchronize_review_selection([entity], [box])
    assert entities[0]["selected"] is False
    assert boxes[0]["selected"] is False

    # Deselecting the parent region must also prevent its linked entity from
    # being masked, while a manually drawn overlapping region remains usable.
    entity["selected"] = True
    box["selected"] = False
    entities, boxes = _synchronize_review_selection([entity], [box])
    assert entities[0]["selected"] is False
    assert boxes[0]["selected"] is False

    manual = {
        "id": "manual-1",
        "source": "manual",
        "type": "MANUAL",
        "bbox": box["bbox"],
        "selected": True,
    }
    entity["selected"] = False
    entities, boxes = _synchronize_review_selection([entity], [manual])
    assert entities[0]["selected"] is False
    assert boxes[0]["selected"] is True


def test_unknown_task_reads_do_not_create_directories():
    mask_id = uuid.uuid4().hex
    restore_id = uuid.uuid4().hex
    assert not (TASKS / mask_id).exists()
    assert not (TASKS / restore_id).exists()
    with TestClient(app) as client:
        masked = client.post(
            "/api/files/mask",
            json={
                "analysis_id": mask_id,
                "filename": "missing.txt",
                "entities": [],
                "reviewed": True,
            },
        )
        restored = client.post(
            f"/api/tasks/{restore_id}/restore",
            json={"password": "fixture-password-123"},
        )
    assert masked.status_code == 404
    assert restored.status_code in {404, 409}
    assert not (TASKS / mask_id).exists()
    assert not (TASKS / restore_id).exists()
