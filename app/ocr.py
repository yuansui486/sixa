from __future__ import annotations

import os
from pathlib import Path


class OCRService:
    def __init__(self, model_dir: Path):
        self.model_dir = model_dir
        self.engine = None
        self.error = None
    def load(self):
        try:
            self.model_dir.mkdir(parents=True, exist_ok=True)
            os.environ['PADDLE_HOME'] = str(self.model_dir)
            os.environ['PADDLEOCR_HOME'] = str(self.model_dir)
            os.environ['FLAGS_use_mkldnn'] = '0'
            os.environ['FLAGS_enable_mkldnn'] = '0'
            os.environ['OMP_NUM_THREADS'] = '1'
            from paddleocr import PaddleOCR
            self.engine = PaddleOCR(
                use_angle_cls=False,
                lang='ch',
                use_gpu=False,
                enable_mkldnn=False,
                cpu_threads=1,
                show_log=False,
            )
            # PaddlePaddle 3.x may keep MKL-DNN enabled on Windows even when
            # constructor flags are false. Explicitly disable it on every
            # predictor to avoid the fused_conv2d Filter error.
            for component in ('text_detector', 'text_recognizer', 'text_classifier'):
                predictor = getattr(self.engine, component, None)
                config = getattr(predictor, 'config', None)
                if config is not None and hasattr(config, 'disable_mkldnn'):
                    config.disable_mkldnn()
            self.error = None
        except Exception as exc:
            self.error = str(exc); self.engine = None
    def analyze(self, image_path: str):
        if not self.engine:
            return []
        try:
            result = self.engine.ocr(image_path, cls=False) or []
        except Exception as exc:
            self.error = f'OCR 推理失败: {exc}'
            self.engine = None
            raise RuntimeError(self.error) from exc
        rows = result[0] if result and isinstance(result[0], list) else result
        boxes=[]
        for item in rows:
            if not isinstance(item, (list, tuple)) or len(item) < 2: continue
            points, value = item[0], item[1]; text, score = value[0], float(value[1])
            xs=[p[0] for p in points]; ys=[p[1] for p in points]
            boxes.append({'text':text,'score':score,'x1':min(xs),'y1':min(ys),'x2':max(xs),'y2':max(ys)})
        return boxes
