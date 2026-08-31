"""Platform-level privacy and validation regressions."""

from __future__ import annotations

import io
import inspect
import tomllib
from copy import deepcopy
from pathlib import Path

import fitz
from fastapi.testclient import TestClient
from PIL import Image, ImageDraw

from app import main
from app.ocr import OCRService
from app.policies import DEFAULT_POLICIES


def test_api_and_package_versions_match() -> None:
    project = tomllib.loads((Path(main.__file__).parents[1] / "pyproject.toml").read_text(encoding="utf-8"))
    assert main.app.version == project["project"]["version"]


def test_task_filter_requests_are_not_dropped_while_history_loads() -> None:
    html = (Path(main.__file__).parent / "static" / "index.html").read_text(
        encoding="utf-8"
    )
    assert "if(state.tasks.loading)return" not in html
    assert "requestId:0" in html
    assert "const requestId=++state.tasks.requestId" in html
    assert "localStorage.setItem(BATCH_STORAGE_KEY,id)" in html
    assert "function resumeBatch(taskId)" in html


def test_ocr_initialization_is_idempotent(tmp_path, monkeypatch) -> None:
    service = OCRService(tmp_path / "ocr")
    builds = []

    def build() -> None:
        builds.append(True)
        service.engine = object()
        service.error = None

    monkeypatch.setattr(service, "_build_engine", build)
    service.load()
    service.load()
    assert service.available
    assert len(builds) == 1
    service._executor.shutdown(wait=True)


def test_cpu_bound_upload_routes_run_in_fastapi_threadpool() -> None:
    routes = {
        route.path: route.endpoint
        for route in main.app.routes
        if hasattr(route, "endpoint")
    }
    for path in (
        "/api/image/analyze",
        "/api/document/analyze",
        "/api/files/analyze",
        "/api/batches",
    ):
        assert not inspect.iscoroutinefunction(routes[path]), path


def _scanned_pdf() -> bytes:
    image = Image.new("RGB", (500, 140), "white")
    ImageDraw.Draw(image).text((30, 50), "13800138000", fill="black")
    stream = io.BytesIO()
    image.save(stream, "PNG")
    document = fitz.open()
    page = document.new_page(width=500, height=140)
    page.insert_image(page.rect, stream=stream.getvalue())
    data = document.tobytes()
    document.close()
    return data


def test_password_protected_pdf_is_rejected_with_clear_error() -> None:
    document = fitz.open()
    document.new_page().insert_text((72, 72), "fixture")
    encrypted = document.tobytes(
        encryption=fitz.PDF_ENCRYPT_AES_256,
        owner_pw="fixture-owner-password",
        user_pw="fixture-user-password",
    )
    document.close()
    with TestClient(main.app) as client:
        response = client.post(
            "/api/files/analyze",
            files={"file": ("protected.pdf", encrypted, "application/pdf")},
        )
    assert response.status_code == 415
    assert response.json()["detail"] == "不支持密码保护的 PDF 文件"


def test_scanned_pdf_requires_ready_ocr_before_masking(monkeypatch) -> None:
    monkeypatch.setattr(main.ocr_service, "engine", None)
    monkeypatch.setattr(main.ocr_service, "_load_requested", False)
    with TestClient(main.app) as client:
        analyzed = client.post(
            "/api/files/analyze",
            files={"file": ("scan.pdf", _scanned_pdf(), "application/pdf")},
        )
        assert analyzed.status_code == 200, analyzed.text
        payload = analyzed.json()
        assert any(item["code"] == "OCR_UNAVAILABLE" for item in payload["warnings"])
        masked = client.post(
            "/api/files/mask",
            json={
                "analysis_id": payload["analysis_id"],
                "filename": payload["filename"],
                "text": payload["text"],
                "entities": payload["entities"],
                "boxes": payload.get("boxes", []),
                "reviewed": True,
            },
        )
        assert masked.status_code == 409


def test_oversized_image_pixels_are_rejected(monkeypatch) -> None:
    monkeypatch.setattr(main, "MAX_IMAGE_PIXELS", 100)
    image = Image.new("RGB", (11, 10), "white")
    source = io.BytesIO()
    image.save(source, "PNG")
    with TestClient(main.app) as client:
        response = client.post(
            "/api/files/analyze",
            files={"file": ("large.png", source.getvalue(), "image/png")},
        )
    assert response.status_code == 413


def test_image_analysis_failure_rolls_back_task_files_and_database(monkeypatch) -> None:
    image = Image.new("RGB", (32, 24), "white")
    source = io.BytesIO()
    image.save(source, "PNG")

    before_directories = {path.name for path in main.TASKS.iterdir() if path.is_dir()}
    connection = main.conn()
    before_rows = {row[0] for row in connection.execute("SELECT id FROM tasks")}
    connection.close()

    def fail_analysis(data: bytes, folder) -> tuple:  # type: ignore[no-untyped-def]
        assert data.startswith(b"\x89PNG")
        assert (folder / "source.bin").is_file()
        raise RuntimeError("fixture OCR failure")

    monkeypatch.setattr(main, "_analyze_image_content", fail_analysis)
    with TestClient(main.app) as client:
        response = client.post(
            "/api/files/analyze",
            files={"file": ("failed.png", source.getvalue(), "image/png")},
        )

    assert response.status_code == 415
    assert response.json()["detail"] == "文件解析失败: fixture OCR failure"
    assert {path.name for path in main.TASKS.iterdir() if path.is_dir()} == before_directories
    connection = main.conn()
    after_rows = {row[0] for row in connection.execute("SELECT id FROM tasks")}
    connection.close()
    assert after_rows == before_rows


def test_new_text_tasks_do_not_store_content_in_sqlite() -> None:
    value = "电话 13800138000"
    with TestClient(main.app) as client:
        entities = client.post("/api/text/analyze", json={"text": value}).json()["entities"]
        masked = client.post(
            "/api/text/mask", json={"text": value, "entities": entities}
        )
        assert masked.status_code == 200, masked.text
        task_id = masked.json()["task_id"]
        connection = main.conn()
        row = connection.execute(
            "SELECT original,masked,source_path,artifact_name FROM tasks WHERE id=?",
            (task_id,),
        ).fetchone()
        connection.close()
        assert row[:2] == ("", "")
        assert (main.TASKS / task_id / row[2]).read_text(encoding="utf-8") == value
        assert "13800138000" not in (main.TASKS / task_id / row[3]).read_text(encoding="utf-8")


def test_policy_configuration_survives_process_cache_reset() -> None:
    original = deepcopy(main.policy_store)
    configured = {
        **deepcopy(DEFAULT_POLICIES["PHONE"]),
        "text_action": "replace",
        "replacement": "本地号码",
    }
    try:
        with TestClient(main.app) as client:
            response = client.put("/api/policies", json={"PHONE": configured})
            assert response.status_code == 200, response.text
            main.policy_store.clear()
            main.policy_store.update(deepcopy(DEFAULT_POLICIES))
            main._POLICY_STORE_LOADED = False
            loaded = client.get("/api/policies")
            assert loaded.status_code == 200, loaded.text
            assert loaded.json()["policies"]["PHONE"]["replacement"] == "本地号码"
    finally:
        main.policy_store.clear()
        main.policy_store.update(original)
        main._POLICY_STORE_LOADED = True
        main._persist_policy_store()
