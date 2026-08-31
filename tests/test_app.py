from io import BytesIO
from fastapi.testclient import TestClient
from PIL import Image
from app.main import app, valid_id, luhn, valid_ip

client = TestClient(app)

def test_validators():
    assert valid_id("11010519491231002X")
    assert not valid_id("110105194912310021")
    assert luhn("4532015112830366")
    assert not luhn("4532015112830367")
    assert valid_ip("192.168.1.1")
    assert not valid_ip("999.1.1.1")

def test_text_roundtrip_and_selection():
    text = "张三电话13800138000，邮箱 a@test.com"
    entities = client.post("/api/text/analyze", json={"text": text}).json()["entities"]
    entities[0]["selected"] = False
    result = client.post("/api/text/mask", json={"text": text, "entities": entities}).json()
    assert "13800138000" in result["masked_text"]
    assert "a@test.com" not in result["masked_text"]
    restored = client.post("/api/text/unmask", json={"task_id": result["task_id"]}).json()
    assert restored["text"] == text

def test_invalid_entity_offsets():
    response = client.post("/api/text/mask", json={"text": "abc", "entities": [{"type": "X", "text": "bad", "start": 0, "end": 3}]})
    assert response.status_code == 422

def test_custom_rule_crud():
    created = client.post("/api/rules", json={"name": "internal", "kind": "word", "pattern": "ProjectX", "entity_type": "PROJECT"})
    assert created.status_code == 200
    entities = client.post("/api/text/analyze", json={"text": "ProjectX"}).json()["entities"]
    assert entities[0]["type"] == "PROJECT"
    assert client.delete(f"/api/rules/{created.json()['id']}").status_code == 200

def test_image_mask():
    image = Image.new("RGB", (100, 100), "white")
    source = BytesIO(); image.save(source, format="PNG"); source.seek(0)
    analysis = client.post("/api/image/analyze", files={"file": ("a.png", source.getvalue(), "image/png")}).json()
    masked = client.post("/api/image/mask", json={"analysis_id": analysis["analysis_id"], "boxes": [{"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5}]}).json()
    assert masked["artifact"].endswith("masked.png")
    assert client.get(masked["artifact"]).status_code == 200

def test_real_ocr_smoke():
    image = Image.new("RGB", (500, 120), "white")
    from PIL import ImageDraw
    ImageDraw.Draw(image).text((20, 40), "13800138000", fill="black")
    source = BytesIO(); image.save(source, format="PNG")
    response = client.post("/api/image/analyze", files={"file": ("ocr.png", source.getvalue(), "image/png")})
    assert response.status_code == 200
    payload = response.json()
    if payload["ocr_available"]:
        assert any("13800138000" in box["text"] for box in payload["boxes"])

def test_real_raner_smoke():
    from app.main import ner_service
    ner_service.load(download=False, device="cpu")
    if ner_service.available:
        entities = ner_service.analyze("张三在北京大学工作。")
        assert any(e["type"] in {"PERSON", "ORGANIZATION", "LOCATION", "GPE"} for e in entities)

def test_document_analyze_txt():
    response = client.post("/api/document/analyze", files={"file": ("a.txt", "电话13800138000".encode(), "text/plain")})
    assert response.status_code == 200
    assert response.json()["entities"][0]["type"] == "PHONE"
