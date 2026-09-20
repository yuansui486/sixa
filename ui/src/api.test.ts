import { describe, it, expect } from "vitest";
import {
  capabilityReady,
  formatBytes,
  selection,
  message,
  regionUpdate,
  type Entity,
} from "./api";
describe("IPC boundary", () => {
  it("submits only entity IDs and decisions, never source text or offsets", () => {
    const entity: Entity = {
      id: "id",
      selected: true,
      replacement: null,
      text: "张三",
      entity_type: "PERSON",
      type_label: "姓名",
      score: 1,
      source: "RaNER",
      display: { start: 2, end: 4 },
    };
    expect(selection([entity])).toEqual([
      { id: "id", selected: true, replacement: null },
    ]);
  });
  it("shows structured native errors in Chinese", () => {
    expect(message({ code: "MODELS_NOT_READY", message: "请安装模型" })).toBe(
      "请安装模型",
    );
  });
  it("builds normalized manual region payloads without source document data", () => {
    const payload = regionUpdate("task-1", {
      id: "region-1",
      page: 0,
      polygon: [
        { x: 0.1, y: 0.2 },
        { x: 0.4, y: 0.2 },
        { x: 0.4, y: 0.5 },
        { x: 0.1, y: 0.5 },
      ],
      entity_id: null,
      selected: true,
      source: "manual",
      text: "",
      score: null,
      rotation: 0,
      replacement: "已脱敏",
    });
    expect(JSON.stringify(payload)).not.toContain("sourceBytes");
    expect(payload.id).toBe("task-1");
    expect(payload.region.polygon).toHaveLength(4);
  });
  it("uses per-capability readiness and formats file sizes for novice-facing views", () => {
    const model = {
      ready: true,
      version: "1",
      location: "models",
      bytes: 1024,
      error: null,
      capabilities: [
        {
          id: "ppocrv4-mobile-v1",
          label: "轻量 OCR",
          installed: true,
          ready: false,
          version: "1",
          bytes: 1024,
          location: "models/ocr",
          error: "校验失败",
        },
      ],
    };
    expect(capabilityReady(model, "raner-v1")).toBe(true);
    expect(capabilityReady(model, "ppocrv4-mobile-v1")).toBe(false);
    expect(formatBytes(1536)).toBe("1.5 KB");
  });
});
