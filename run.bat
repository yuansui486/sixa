@echo off
cd /d "%~dp0"
uv sync --extra ai --extra ocr --extra dev
if errorlevel 1 pause & exit /b 1
uv run uvicorn app.main:app --host 127.0.0.1 --port 8765
pause
