"""Batch restart, privacy, failure, and cleanup regressions."""

from __future__ import annotations

import io
import json
import time
import zipfile
from fastapi.testclient import TestClient

from app import main


def _wait_for(client: TestClient, batch_id: str, statuses: set[str]) -> dict:
    state: dict = {}
    for _ in range(300):
        response = client.get(f"/api/batches/{batch_id}")
        assert response.status_code == 200, response.text
        state = response.json()
        if state.get("status") in statuses:
            return state
        time.sleep(0.02)
    raise AssertionError(f"batch did not reach {statuses}: {state}")


def _create_text_batch(client: TestClient) -> tuple[str, dict]:
    response = client.post(
        "/api/batches",
        files={"files": ("private.txt", "电话 13800138000".encode(), "text/plain")},
        data={"options": "{}"},
    )
    assert response.status_code == 200, response.text
    batch_id = response.json()["batch_id"]
    return batch_id, _wait_for(client, batch_id, {"awaiting_review"})


def test_batch_sqlite_snapshot_excludes_plaintext_and_hydrates_after_restart() -> None:
    with TestClient(main.app) as client:
        batch_id, state = _create_text_batch(client)
        assert state["items"][0]["entities"][0]["text"] == "13800138000"

        connection = main.conn()
        metadata = connection.execute(
            "SELECT metadata_json FROM batches WHERE id=?", (batch_id,)
        ).fetchone()[0]
        connection.close()
        assert "13800138000" not in metadata
        assert "replacement" not in metadata
        assert "password" not in metadata

        with main.batch_lock:
            main.batches.pop(batch_id, None)
        reloaded = client.get(f"/api/batches/{batch_id}")
        assert reloaded.status_code == 200, reloaded.text
        restored = reloaded.json()
        assert restored["status"] == "awaiting_review"
        assert restored["items"][0]["text"] == "电话 13800138000"
        assert restored["items"][0]["entities"][0]["text"] == "13800138000"


def test_corrupt_xlsx_batch_finishes_and_leaves_no_child_source() -> None:
    broken = io.BytesIO()
    with zipfile.ZipFile(broken, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", "<Types>")
        archive.writestr("xl/workbook.xml", "<workbook>")

    with TestClient(main.app) as client:
        response = client.post(
            "/api/batches",
            files={
                "files": (
                    "broken.xlsx",
                    broken.getvalue(),
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                )
            },
            data={"options": "{}"},
        )
        assert response.status_code == 200, response.text
        batch_id = response.json()["batch_id"]
        state = _wait_for(client, batch_id, {"failed"})
        assert state["failed"] == 1

        connection = main.conn()
        children = connection.execute(
            "SELECT id FROM tasks WHERE batch_id=?", (batch_id,)
        ).fetchall()
        connection.close()
        assert children == []
        batch_folder = main.TASKS / batch_id
        assert not (batch_folder / "inputs").exists()
        assert not any(path.is_dir() for path in batch_folder.iterdir())


def test_cancel_batch_removes_inputs_and_child_analysis() -> None:
    with TestClient(main.app) as client:
        batch_id, state = _create_text_batch(client)
        task_id = state["items"][0]["task_id"]
        response = client.post(f"/api/batches/{batch_id}/cancel")
        assert response.status_code == 200, response.text
        assert response.json()["status"] == "cancelled"

        assert not (main.TASKS / batch_id / "inputs").exists()
        child = main.TASKS / task_id
        assert not (child / "source.bin").exists()
        assert not (child / "analysis.json").exists()
        connection = main.conn()
        row = connection.execute(
            "SELECT status,source_path FROM tasks WHERE id=?", (task_id,)
        ).fetchone()
        connection.close()
        assert row == ("cancelled", "")


def test_completed_batch_zip_has_outputs_without_duplicate_batch_files() -> None:
    with TestClient(main.app) as client:
        batch_id, state = _create_text_batch(client)
        response = client.post(
            f"/api/batches/{batch_id}/mask",
            json={"reviewed": True, "items": state["items"]},
        )
        assert response.status_code == 200, response.text
        finished = _wait_for(client, batch_id, {"completed", "partial", "failed"})
        assert finished["status"] == "completed", finished
        archive_path = main.TASKS / batch_id / "results.zip"
        with zipfile.ZipFile(archive_path) as archive:
            assert "private_masked.txt" in archive.namelist()
            report = json.loads(archive.read("report.json"))
            assert "13800138000" not in json.dumps(report, ensure_ascii=False)
        remaining = {path.name for path in (main.TASKS / batch_id).iterdir() if path.is_file()}
        assert remaining == {"report.json", "results.zip"}

        history = client.get("/api/tasks?kind=batch&status=completed").json()
        parent = next(item for item in history["tasks"] if item["id"] == batch_id)
        assert parent["artifact"] == "results.zip"
        assert parent["filename"] == "批量任务（1 个文件）"
        assert client.get(
            f"/api/tasks/{batch_id}/artifacts/{parent['artifact']}"
        ).status_code == 200

        child_ids = [item["task_id"] for item in finished["items"] if item.get("task_id")]
        deleted = client.delete(f"/api/tasks/{batch_id}")
        assert deleted.status_code == 200, deleted.text
        assert deleted.json()["children_deleted"] == len(child_ids)
        assert not (main.TASKS / batch_id).exists()
        assert all(not (main.TASKS / task_id).exists() for task_id in child_ids)
        assert client.get(f"/api/batches/{batch_id}").status_code == 404


def test_batch_archive_preserves_safe_chinese_filename() -> None:
    with TestClient(main.app) as client:
        response = client.post(
            "/api/batches",
            files={"files": ("客户资料.txt", "电话 13800138000".encode(), "text/plain")},
            data={"options": "{}"},
        )
        assert response.status_code == 200, response.text
        batch_id = response.json()["batch_id"]
        reviewed = _wait_for(client, batch_id, {"awaiting_review"})
        execute = client.post(
            f"/api/batches/{batch_id}/mask",
            json={"reviewed": True, "items": reviewed["items"]},
        )
        assert execute.status_code == 200, execute.text
        finished = _wait_for(client, batch_id, {"completed", "partial", "failed"})
        assert finished["status"] == "completed", finished
        with zipfile.ZipFile(main.TASKS / batch_id / "results.zip") as archive:
            assert "客户资料_masked.txt" in archive.namelist()


def test_cleanup_expired_batch_also_removes_newer_child_task() -> None:
    with TestClient(main.app) as client:
        batch_id, state = _create_text_batch(client)
        task_id = state["items"][0]["task_id"]
        connection = main.conn()
        connection.execute(
            "UPDATE batches SET created=0,updated=0 WHERE id=?", (batch_id,)
        )
        connection.commit()
        connection.close()

        main.cleanup()
        connection = main.conn()
        batch_row = connection.execute(
            "SELECT id FROM batches WHERE id=?", (batch_id,)
        ).fetchone()
        task_row = connection.execute(
            "SELECT id FROM tasks WHERE id=?", (task_id,)
        ).fetchone()
        connection.close()
        assert batch_row is None
        assert task_row is None
        assert not (main.TASKS / batch_id).exists()
        assert not (main.TASKS / task_id).exists()
