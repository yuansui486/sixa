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

    @property
    def available(self):
        return self.pipeline is not None

    def load(self, download=True, device="cpu"):
        with self.lock:
            if self.pipeline is not None:
                return
            try:
                from modelscope import snapshot_download
                if download and not self.model_dir.exists():
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
        for start in range(0, len(text), 450):
            offset = start
            chunk = text[start:start + 450]
            try:
                output = self.pipeline(chunk).get("output", [])
            except Exception:
                continue
            for item in output:
                typ = TYPE_MAP.get(item.get("type"), item.get("type", "CUSTOM"))
                s, e = int(item.get("start", 0)) + offset, int(item.get("end", 0)) + offset
                if e > s:
                    results.append({"type": typ, "text": text[s:e], "start": s, "end": e, "score": float(item.get("prob", .7)), "recognizer": "raner", "source": "ner"})
        return results
