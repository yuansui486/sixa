uv sync --extra ai --extra ocr --extra dev
uv run uvicorn app.main:app --host 127.0.0.1 --port 8765
