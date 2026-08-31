from __future__ import annotations

import threading
from pathlib import Path

MODEL_ID = "iic/nlp_raner_named-entity-recognition_chinese-base-generic"
TYPE_MAP = {"PER": "PERSON", "ORG": "ORGANIZATION", "LOC": "LOCATION", "GPE": "GPE"}

class RaanerService:
    def __init__(self, model_dir: Path):
        self.model_dir = model_dir
        self.pipeline = None
        self.error = None
        self.lock = threading.Lock()
        self.inference_lock = threading.Lock()

    @property
    def available(self):
        return self.pipeline is not None

    def load(self, download=True, device="cpu"):
        with self.lock:
            if self.pipeline is not None:
                return
            try:
                from modelscope import snapshot_download
                required = (
                    self.model_dir / "configuration.json",
                    self.model_dir / "pytorch_model.bin",
                    self.model_dir / "vocab.txt",
                )
                if download and not all(path.is_file() for path in required):
                    snapshot_download(MODEL_ID, local_dir=str(self.model_dir), cache_dir=str(self.model_dir.parent / "cache"))
                from modelscope.pipelines import pipeline
                from modelscope.utils.constant import Tasks
                self.pipeline = pipeline(Tasks.named_entity_recognition, model=str(self.model_dir), device=device)
                self.error = None
            except Exception as exc:
                self.error = str(exc)
                self.pipeline = None

    def analyze(self, text: str):
        if not self.pipeline:
            return []
        results = []
        seen: set[tuple[str, int, int]] = set()
        errors: list[str] = []
        # Overlap adjacent windows so names split around the model's sequence
        # boundary still appear intact in at least one inference request.
        chunk_size = 450
        chunk_step = 400
        with self.inference_lock:
            for start in range(0, len(text), chunk_step):
                chunk = text[start:start + chunk_size]
                if not chunk:
                    continue
                try:
                    response = self.pipeline(chunk)
                    output = response.get("output", []) if isinstance(response, dict) else []
                except Exception as exc:
                    errors.append(str(exc))
                    continue
                for item in output:
                    if not isinstance(item, dict):
                        continue
                    raw_type = str(item.get("type", "CUSTOM"))
                    typ = TYPE_MAP.get(raw_type, raw_type)
                    try:
                        entity_start = int(item.get("start", 0)) + start
                        entity_end = int(item.get("end", 0)) + start
                        score = float(item.get("prob", .7))
                    except (TypeError, ValueError):
                        continue
                    if entity_start < 0 or entity_end <= entity_start or entity_end > len(text):
                        continue
                    key = (typ, entity_start, entity_end)
                    if key in seen:
                        continue
                    seen.add(key)
                    results.append({
                        "type": typ,
                        "text": text[entity_start:entity_end],
                        "start": entity_start,
                        "end": entity_end,
                        "score": max(0.0, min(1.0, score)),
                        "recognizer": "raner",
                        "source": "ner",
                    })
        if errors:
            self.error = f"NER 推理失败: {errors[0]}"
        else:
            self.error = None
        return sorted(results, key=lambda item: (item["start"], -item["end"]))
