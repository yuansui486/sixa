from __future__ import annotations

import gc
import os
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any


class OCRService:
    """Own a PaddleOCR predictor on one dedicated worker thread.

    Paddle's Windows CPU predictor keeps thread-local oneDNN state. FastAPI
    sync handlers may run consecutive requests on different worker threads,
    which can leave a reused predictor without its convolution filters. Both
    construction and inference therefore run on this service's single worker.
    """

    def __init__(self, model_dir: Path):
        self.model_dir = model_dir
        self.engine: Any | None = None
        self.error: str | None = None
        self._load_requested = False
        self._load_lock = threading.Lock()
        self._executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="paddle-ocr")

    @property
    def enabled(self) -> bool:
        """Return whether the user has requested OCR model initialization."""

        return self._load_requested

    @property
    def available(self) -> bool:
        return self.engine is not None

    def _configure_environment(self) -> None:
        self.model_dir.mkdir(parents=True, exist_ok=True)
        os.environ["PADDLE_HOME"] = str(self.model_dir)
        os.environ["PADDLEOCR_HOME"] = str(self.model_dir)
        os.environ["FLAGS_use_mkldnn"] = "0"
        os.environ["FLAGS_enable_mkldnn"] = "0"
        os.environ["OMP_NUM_THREADS"] = "1"

    def _build_engine(self) -> None:
        self._configure_environment()
        from paddleocr import PaddleOCR

        engine = PaddleOCR(
            use_angle_cls=False,
            lang="ch",
            use_gpu=False,
            enable_mkldnn=False,
            cpu_threads=1,
            show_log=False,
        )
        # Some PaddlePaddle 3.x Windows wheels retain MKL-DNN in the native
        # predictor despite the constructor option. Disable it explicitly on
        # every component before exposing the engine to inference calls.
        for component in ("text_detector", "text_recognizer", "text_classifier"):
            predictor = getattr(engine, component, None)
            config = getattr(predictor, "config", None)
            if config is not None and hasattr(config, "disable_mkldnn"):
                config.disable_mkldnn()
        self.engine = engine
        self.error = None

    def _load_on_worker(self) -> None:
        self.engine = None
        gc.collect()
        self._build_engine()

    def load(self) -> None:
        with self._load_lock:
            self._load_requested = True
            if self.engine is not None:
                self.error = None
                return
            try:
                self._executor.submit(self._load_on_worker).result()
            except Exception as exc:
                self.engine = None
                self.error = f"OCR 初始化失败: {exc}"

    def _infer(self, image_path: str) -> Any:
        if self.engine is None:
            self._build_engine()
        return self.engine.ocr(image_path, cls=False) or []

    def _analyze_on_worker(self, image_path: str) -> list[dict[str, Any]]:
        first_error: Exception | None = None
        for attempt in range(2):
            try:
                result = self._infer(image_path)
                self.error = None
                return self._parse_result(result)
            except Exception as exc:
                first_error = first_error or exc
                self.engine = None
                gc.collect()
                if attempt == 0:
                    # Recreate every native predictor on this same worker and
                    # retry once. A transient primitive/cache failure should
                    # not disable OCR for all subsequent requests.
                    continue
                self.error = f"OCR 推理失败: {first_error}; 重试失败: {exc}"
                raise RuntimeError(self.error) from exc
        return []

    def analyze(self, image_path: str) -> list[dict[str, Any]]:
        if not self._load_requested and self.engine is None:
            return []
        return self._executor.submit(self._analyze_on_worker, image_path).result()

    @staticmethod
    def _parse_result(result: Any) -> list[dict[str, Any]]:
        rows = result[0] if result and isinstance(result[0], list) else result
        boxes: list[dict[str, Any]] = []
        for item in rows or []:
            if not isinstance(item, (list, tuple)) or len(item) < 2:
                continue
            points, value = item[0], item[1]
            if not isinstance(value, (list, tuple)) or len(value) < 2:
                continue
            try:
                text, score = str(value[0]), float(value[1])
                xs = [float(point[0]) for point in points]
                ys = [float(point[1]) for point in points]
            except (TypeError, ValueError, IndexError):
                continue
            if not xs or not ys:
                continue
            boxes.append(
                {
                    "text": text,
                    "score": score,
                    "x1": min(xs),
                    "y1": min(ys),
                    "x2": max(xs),
                    "y2": max(ys),
                }
            )
        return boxes
