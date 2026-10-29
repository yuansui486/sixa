import { expect, test, type Page } from "@playwright/test";

declare global {
  interface Window {
    __mock__: {
      calls: Record<string, number>;
      task: any;
      lastUpsert: any;
      lastRemove: any;
      closeReplies: any[];
      resolveDocumentPreview?: () => void;
      resolveUpsert?: () => void;
      resolveRemove?: () => void;
      resolveReviewPatch?: () => void;
      emit: (event: string, payload: unknown) => void;
    };
    __delayDocumentPreview__?: boolean;
    __delayRegionMutations__?: boolean;
    __delayReviewPatch__?: boolean;
    __rebuild_count__?: number;
    __auth_status__?: any;
    __copied_text__?: string;
    __mcp_path__?: string;
  }
}

const PDF_PAGE =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='600' height='840'%3E%3Crect width='600' height='840' fill='%23fff'/%3E%3Ctext x='48' y='72' font-size='28'%3E%E7%AE%80%E5%8E%86%3C/text%3E%3C/svg%3E";

test.beforeEach(async ({ page }) => {
  await page.addInitScript((previewUri) => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async (value: string) => {
          window.__copied_text__ = value;
        },
      },
    });
    let task: any = {
      meta: {
        id: "task-pdf-1",
        state: "awaiting_review",
        kind: "pdf",
        created_at: 1,
        updated_at: 1,
        error: null,
        display_name: "测试简历.pdf",
      },
      text: "OCR_TEXT_MUST_NOT_FLASH 张三在北京",
      extension: "pdf",
      preview: null,
      entities: [
        {
          id: "entity-1",
          entity_type: "PERSON",
          type_label: "姓名",
          score: 0.99,
          source: "RaNER",
          selected: true,
          display: { start: 24, end: 26 },
          text: "张三",
          replacement: null,
        },
      ],
      regions: [
        {
          id: "entity-region-1",
          page: 0,
          polygon: [
            { x: 0.1, y: 0.12 },
            { x: 0.24, y: 0.12 },
            { x: 0.24, y: 0.17 },
            { x: 0.1, y: 0.17 },
          ],
          entity_id: "entity-1",
          selected: true,
          source: "entity",
          text: "张三",
          score: 0.99,
          rotation: 0,
          replacement: "某人",
        },
      ],
      options: { ocr_profile: "mobile", pdf_mode: "safe_rebuild" },
      warnings: [],
      revision: 1,
    };
    const documentPreview = {
      revision: 1,
      pages: [
        {
          index: 0,
          width: 600,
          height: 840,
          preview_uri: previewUri,
        },
      ],
      text: null,
      warnings: [],
    };
    const eventCallbacks = new Map<number, (event: unknown) => void>();
    const eventListeners = new Map<string, Set<number>>();
    let nextCallbackId = 1;
    let closeRequest: any = null;
    let desktopPreferences = { close_behavior: "ask", tray_available: true };
    const state = (window.__mock__ = {
      calls: {},
      task,
      lastUpsert: null,
      lastRemove: null,
      closeReplies: [] as any[],
      emit: (event: string, payload: unknown) => {
        if (event === "app-close-request") closeRequest = payload;
        for (const id of eventListeners.get(event) ?? []) {
          eventCallbacks.get(id)?.({ event, id, payload });
        }
      },
    });
    const count = (command: string) => {
      state.calls[command] = (state.calls[command] ?? 0) + 1;
    };
    const ackUpsert = (args: any) => {
      const existing = task.regions.findIndex(
        (region: any) => region.id === args.region.id,
      );
      if (existing >= 0) task.regions[existing] = args.region;
      else task.regions.push(args.region);
      task.revision += 1;
      state.task = task;
      return { revision: task.revision, region_id: args.region.id };
    };
    const ackRemove = (args: any) => {
      task.regions = task.regions.filter(
        (region: any) => region.id !== args.regionId,
      );
      task.revision += 1;
      state.task = task;
      return { revision: task.revision, region_id: args.regionId };
    };
    const handlers: Record<string, (args: any) => unknown> = {
      auth_status: () =>
        window.__auth_status__ ?? {
          authenticated: true,
          offline: false,
          offline_until: 4_102_444_800,
          reason: null,
          subject: {
            id: "user-1",
            username: "tester",
            display_name: "测试用户",
            tenant_id: "tenant-1",
            tenant_code: "demo",
            tenant_name: "测试租户",
          },
          policy: {
            product_code: "data-desensitization",
            module_enabled: true,
            concurrent_device_limit: 1,
            active_session_count: 1,
            heartbeat_interval_seconds: 600,
            offline_grace_seconds: 86400,
          },
        },
      auth_login: () => {
        window.__auth_status__ = undefined;
        return handlers.auth_status({});
      },
      auth_logout: () => undefined,
      get_desktop_preferences: () => desktopPreferences,
      get_app_update_status: () => ({
        revision: 1,
        current_version: "1.0.9",
        version: null,
        notes: null,
        phase: "idle",
        automatic: true,
        last_check: 0,
        downloaded: 0,
        total: null,
        bytes_per_second: 0,
        eta_seconds: null,
        error: null,
      }),
      check_app_update: () => ({
        revision: 2,
        current_version: "1.0.9",
        version: "1.0.10",
        notes: "改进文档预览",
        phase: "available",
        automatic: true,
        last_check: 1,
        downloaded: 0,
        total: null,
        bytes_per_second: 0,
        eta_seconds: null,
        error: null,
      }),
      download_app_update: () => ({
        revision: 3,
        current_version: "1.0.9",
        version: "1.0.10",
        notes: "改进文档预览",
        phase: "downloading",
        automatic: true,
        last_check: 1,
        downloaded: 1024 * 1024,
        total: 2 * 1024 * 1024,
        bytes_per_second: 1024 * 1024,
        eta_seconds: 1,
        error: null,
      }),
      set_desktop_preferences: ({ closeBehavior }) => {
        desktopPreferences = {
          ...desktopPreferences,
          close_behavior: closeBehavior,
        };
        return desktopPreferences;
      },
      get_close_request: () => closeRequest,
      acknowledge_app_close: () => undefined,
      respond_app_close: (args) => {
        state.closeReplies.push(args);
        if (!closeRequest || closeRequest.id !== args.requestId)
          return closeRequest;
        if (args.remember && ["tray", "exit"].includes(args.action))
          desktopPreferences.close_behavior = args.action;
        if (["tray", "cancel"].includes(args.action)) closeRequest = null;
        else if (args.action === "exit")
          closeRequest = { ...closeRequest, phase: "saving" };
        else if (
          args.action === "saved" ||
          (args.action === "force" && closeRequest.phase === "saving")
        )
          closeRequest =
            closeRequest.active_tasks || closeRequest.active_downloads
              ? { ...closeRequest, phase: "confirm" }
              : null;
        else if (
          args.action === "stop" ||
          (args.action === "wait" && closeRequest.phase !== "saving")
        )
          closeRequest = { ...closeRequest, phase: "stopping" };
        else if (args.action === "force") closeRequest = null;
        return closeRequest;
      },
      model_status: () => ({
        ready: !new URLSearchParams(location.search).has("delay-model-setup"),
        version: "test-only",
        location: "C:\\test\\models",
        bytes: 100,
        error: null,
        capabilities: [
          {
            id: "raner-v1",
            label: "中文实体识别",
            installed: !new URLSearchParams(location.search).has(
              "delay-model-setup",
            ),
            ready: !new URLSearchParams(location.search).has(
              "delay-model-setup",
            ),
            version: "test-only",
            bytes: 50,
            location: "C:\\test\\models\\raner-v1",
            error: null,
          },
          {
            id: "ppocrv4-mobile-v1",
            label: "轻量 OCR",
            installed: !new URLSearchParams(location.search).has(
              "delay-model-setup",
            ),
            ready: !new URLSearchParams(location.search).has(
              "delay-model-setup",
            ),
            version: "test-only",
            bytes: 50,
            location: "C:\\test\\models\\ppocrv4-mobile-v1",
            error: null,
          },
        ],
      }),
      load_models: () => handlers.model_status({}),
      ensure_default_models: () => {
        if (!new URLSearchParams(location.search).has("delay-model-setup"))
          return handlers.model_status({});
        return new Promise(() => {});
      },
      model_packages: () => [
        {
          id: "raner-v1",
          version: "test-only",
          profile: "text",
          size: 100,
          location: "C:\\test\\models\\raner-v1",
          installed: true,
          ready: true,
          error: null,
        },
      ],
      rebuild_model: () => {
        window.__rebuild_count__ = (window.__rebuild_count__ ?? 0) + 1;
        return handlers.model_status({});
      },
      get_settings: () => ({
        concurrency: 2,
        ocr_profile: "mobile",
        pdf_mode: "safe_rebuild",
      }),
      integration_info: () => ({
        enabled: true,
        mcp_available: true,
        authenticated: true,
        models_ready: true,
        executable_path:
          window.__mcp_path__ ??
          "C:\\Users\\tester\\AppData\\Local\\私匣\\sixa-mcp.exe",
        protocol_version: "1",
        supported_formats: ["TXT", "PDF", "DOCX", "XLSX", "PPTX", "PNG", "JPG"],
      }),
      integration_check: () => ({
        ok: true,
        message: "本机连接正常",
      }),
      analyze_file: () => task,
      task_view: () => task,
      review_patch: ({ selections, expectedRevision }) => {
        if (expectedRevision !== task.revision) throw Error("复核版本冲突");
        task.entities = task.entities.map((entity: any) => ({
          ...entity,
          ...selections.find((item: any) => item.id === entity.id),
        }));
        task.revision += 1;
        state.task = task;
        const snapshot = structuredClone(task);
        if (window.__delayReviewPatch__)
          return new Promise((resolve) => {
            state.resolveReviewPatch = () => {
              window.__delayReviewPatch__ = false;
              resolve(snapshot);
            };
          });
        return snapshot;
      },
      preview: () => "OCR_TEXT_MUST_NOT_FLASH 某人在北京",
      document_manifest: (args) => {
        if (args.result) return documentPreview;
        if (!window.__delayDocumentPreview__) return documentPreview;
        return new Promise((resolve) => {
          state.resolveDocumentPreview = () => resolve(documentPreview);
        });
      },
      document_result_preview: () => documentPreview,
      document_draft_page: () =>
        Uint8Array.from(
          atob(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a6XcAAAAASUVORK5CYII=",
          ),
          (c) => c.charCodeAt(0),
        ).buffer,
      cancel_document_preview: () => undefined,
      upsert_region: (args) => {
        state.lastUpsert = args;
        if (!window.__delayRegionMutations__) return ackUpsert(args);
        return new Promise((resolve) => {
          state.resolveUpsert = () => resolve(ackUpsert(args));
        });
      },
      remove_region: (args) => {
        state.lastRemove = args;
        if (!window.__delayRegionMutations__) return ackRemove(args);
        return new Promise((resolve) => {
          state.resolveRemove = () => resolve(ackRemove(args));
        });
      },
      execute: () => {
        task = {
          ...task,
          meta: { ...task.meta, state: "completed" },
          preview: "某人在北京",
        };
        state.task = task;
        return task;
      },
      export_task: () => undefined,
      get_file_directory: () => "C:\\test\\exports",
      set_file_directory: () => undefined,
      query_output_files: () => ({ items: [], total: 0 }),
      browse_directory: (args: any) => ({
        root: "C:\\test\\exports",
        path: "C:\\test\\exports",
        relative: args.query.relative || "",
        items: [],
        total: 0,
      }),
      create_output_directory: () => "C:/test/exports/new",
      save_result: (args: any) => ({
        id: "saved",
        path: `C:/test/exports/${args.relative ? args.relative + "/" : ""}${args.name}`,
        name: args.name,
      }),
      export_recovery: () => undefined,
      list_tasks: () => [task.meta],
      query_tasks: () => ({
        items: [task.meta],
        total: 1,
        offset: 0,
        limit: 30,
      }),
      confirm_review: () => {
        task.meta.reviewed_revision = task.revision;
        return task;
      },
      retry_model_load: () => handlers.model_status({}),
      list_rules: () => [],
      list_policies: () => [],
      builtin_policies: () => [
        {
          entity_type: "ORGANIZATION",
          type_label: "机构",
          behavior: "学校替换为“某学校”，其余替换为“某公司”",
          examples: [
            { original: "北京大学", replacement: "某学校" },
            { original: "某科技有限公司", replacement: "某公司" },
          ],
        },
      ],
    };
    window.__TAURI_INTERNALS__ = {
      invoke: async (command: string, args: any) => {
        count(command);
        if (command === "plugin:dialog|open") {
          return args?.options?.directory
            ? "C:\\test\\exports"
            : "C:\\test\\测试简历.pdf";
        }
        if (command === "plugin:event|listen") {
          const listeners = eventListeners.get(args.event) ?? new Set<number>();
          listeners.add(args.handler);
          eventListeners.set(args.event, listeners);
          return args.handler;
        }
        if (command === "plugin:event|unlisten") {
          eventListeners.get(args.event)?.delete(args.eventId);
          return undefined;
        }
        const handler = handlers[command];
        if (!handler) throw Error("未模拟命令 " + command);
        return structuredClone(await handler(args));
      },
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { label: "main" },
      },
      transformCallback: (callback: (event: unknown) => void, once = false) => {
        const id = nextCallbackId++;
        eventCallbacks.set(id, (event) => {
          callback(event);
          if (once) eventCallbacks.delete(id);
        });
        return id;
      },
    } as any;
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: () => undefined,
    } as any;
  }, PDF_PAGE);
});

async function selectPdf(page: Page) {
  await page.getByRole("button", { name: "选择本机文件" }).click();
  await expect(page.getByText("测试简历.pdf", { exact: true })).toBeVisible();
}

async function expectNoHorizontalOverflow(page: Page) {
  expect(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth <=
        document.documentElement.clientWidth,
    ),
  ).toBe(true);
}

test("原文标注默认打开，实际效果只在保存完成后按需更新", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await expect(
    page.getByRole("button", { name: "原文标注", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("polygon.entity")).toBeVisible();
  await page.waitForTimeout(700);
  expect(
    await page.evaluate(() => window.__mock__.calls.document_draft_page ?? 0),
  ).toBe(0);
  await page.getByRole("button", { name: "脱敏效果", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(() => window.__mock__.calls.document_draft_page ?? 0),
    )
    .toBe(1);
  await expect(page.locator(".redaction-preview")).toHaveCount(0);
  await page.evaluate(() => {
    window.__delayReviewPatch__ = true;
  });
  await page.getByLabel("选择张三").uncheck();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.review_patch ?? 0))
    .toBe(1);
  await page.waitForTimeout(650);
  expect(
    await page.evaluate(() => window.__mock__.calls.document_draft_page),
  ).toBe(1);
  await page.evaluate(() => window.__mock__.resolveReviewPatch?.());
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.document_draft_page))
    .toBe(2);
});

test("图上定位可临时显示被筛选隐藏的实体", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await page.getByLabel("筛选实体").fill("不匹配的关键词");
  await expect(page.getByLabel("选择张三")).toHaveCount(0);
  await page.locator("polygon.entity").click();
  await expect(page.getByLabel("选择张三")).toBeVisible();
  await expect(page.getByLabel("筛选实体")).toHaveValue("不匹配的关键词");
  await expect(page.locator(".entity-item.focused")).toContainText("张三");
  await expect(page.locator(".entity-context")).toBeVisible();
});

test("对比可调整宽度，框选自动切回原件并聚焦新增区域", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await page.getByRole("button", { name: "对比", exact: true }).click();
  const resize = page.getByRole("separator", { name: "调整对比宽度" });
  await resize.focus();
  await resize.press("ArrowRight");
  await expect(page.locator(".split-preview")).toHaveAttribute("style", /55%/);
  await page.getByRole("button", { name: "框选", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "原文标注", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  const box = (await page.getByTestId("region-canvas-page-0").boundingBox())!;
  await page.mouse.move(box.x + box.width * 0.2, box.y + box.height * 0.2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.4, box.y + box.height * 0.3);
  await page.mouse.up();
  await expect(page.getByLabel("第 1 页区域替换文字")).toBeFocused();
  await expect(page.locator("polygon.manual.focused")).toBeVisible();
  await expectNoHorizontalOverflow(page);
});

test("专注模式收起两侧面板并可恢复", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await page.getByRole("button", { name: "展开侧边栏" }).click();
  await page.getByRole("button", { name: "专注模式", exact: true }).click();
  await expect(page.locator(".app")).toHaveClass(/sidebar-collapsed/);
  await expect(page.getByLabel("复核检查器")).toHaveCount(0);
  await page.getByRole("button", { name: "退出专注模式" }).click();
  await expect(page.getByLabel("复核检查器")).toBeVisible();
  await expect(page.locator(".app")).not.toHaveClass(/sidebar-collapsed/);
});

test("缩放支持 50% 到 300%，新任务清除筛选和框选状态", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  for (let index = 0; index < 8; index++)
    await page.getByLabel("放大", { exact: true }).click();
  await expect(
    page.getByRole("button", { name: "300%", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("放大", { exact: true })).toBeDisabled();
  await expectNoHorizontalOverflow(page);
  await page.getByRole("button", { name: "300%", exact: true }).click();
  await page.getByLabel("缩小", { exact: true }).click();
  await page.getByLabel("缩小", { exact: true }).click();
  await expect(
    page.getByRole("button", { name: "50%", exact: true }),
  ).toBeVisible();
  await page.getByLabel("筛选实体").fill("不存在的内容");
  await page.getByRole("button", { name: "框选", exact: true }).click();
  await page.getByRole("button", { name: "新建任务", exact: true }).click();
  await selectPdf(page);
  await expect(page.getByLabel("筛选实体")).toHaveValue("");
  await expect(
    page.getByRole("button", { name: "框选", exact: true }),
  ).toHaveAttribute("aria-pressed", "false");
  await expect(
    page.getByRole("button", { name: "原文标注", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByLabel("跳转页码")).toHaveValue("1");
});

test("OCR 辅助框可转为手工脱敏区域", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      const result = await old(command, args);
      if (command === "analyze_file")
        result.regions.push({
          ...result.regions[0],
          id: "ocr-only",
          entity_id: null,
          source: "ocr",
          selected: false,
          text: "待检查内容",
          polygon: [
            { x: 0.4, y: 0.2 },
            { x: 0.7, y: 0.2 },
            { x: 0.7, y: 0.25 },
            { x: 0.4, y: 0.25 },
          ],
        });
      return result;
    };
  });
  await selectPdf(page);
  await page.getByLabel("显示 OCR 辅助框").click();
  await page.locator("polygon.ocr").click();
  await expect(page.locator(".helper-detail")).toContainText("待检查内容");
  await page.getByRole("button", { name: "设为脱敏区域" }).click();
  await expect(page.locator("polygon.manual")).toBeVisible();
  await expect(page.getByLabel("第 1 页区域替换文字")).toBeFocused();
});

test("完成后的输出失败显示重试而不回退成原图", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    let failed = false;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "document_manifest" && args.result && !failed) {
        failed = true;
        throw Error("文件暂时无法读取");
      }
      return old(command, args);
    };
  });
  await selectPdf(page);
  await page.getByRole("button", { name: "生成预览", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "重新加载结果" }),
  ).toBeVisible();
  await expect(page.locator(".canvas-scroll img")).toHaveCount(0);
  await expect(page.locator(".redaction-preview")).toHaveCount(0);
  await page.getByRole("button", { name: "重新加载结果" }).click();
  await expect(page.locator(".canvas-scroll img")).toBeVisible();
});

test("带内嵌图片的表格仍默认显示正文，图片不冒充页码", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      const result = await old(command, args);
      if (command === "analyze_file") {
        result.extension = "xlsx";
        result.meta.display_name = "测试表格.xlsx";
      }
      if (command === "document_manifest") {
        result.kind = "office_content";
        result.text = "张三在北京";
      }
      return result;
    };
  });
  await page.getByRole("button", { name: "选择本机文件" }).click();
  await expect(page.locator(".canvas-scroll pre")).toContainText("张三在北京");
  await expect(page.locator(".page-preview")).toHaveCount(0);
  await page.getByRole("button", { name: "内嵌图片 1", exact: true }).click();
  await expect(page.getByAltText("内嵌图片 1")).toBeVisible();
  await expect(page.locator(".page-number")).toHaveText("内嵌图片 1");
  await page.getByRole("button", { name: "单元格与文字", exact: true }).click();
  await expect(page.locator(".canvas-scroll pre")).toContainText("张三在北京");
});

test("真实 DOCX 排版同时显示文字和图片，实体定位与图片复核可往返", async ({
  page,
}, testInfo) => {
  const { default: JSZip } = await import("jszip");
  const archive = new JSZip();
  const picture = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 180;
    canvas.height = 120;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "#247265";
    context.fillRect(0, 0, 180, 120);
    context.fillStyle = "#e8b34d";
    context.fillRect(20, 20, 55, 60);
    context.fillStyle = "#d8ede6";
    context.fillRect(92, 20, 65, 18);
    context.fillRect(92, 48, 48, 10);
    context.fillStyle = "#ffffff";
    context.font = "14px sans-serif";
    context.fillText("项目示意图", 20, 104);
    return canvas.toDataURL("image/png").split(",")[1];
  });
  archive.file(
    "[Content_Types].xml",
    '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
  );
  archive.file(
    "_rels/.rels",
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
  );
  archive.file(
    "word/_rels/document.xml.rels",
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/photo.png"/></Relationships>',
  );
  archive.file(
    "word/document.xml",
    '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body><w:p><w:bookmarkStart w:id="1" w:name="body0"/><w:r><w:rPr><w:sz w:val="32"/></w:rPr><w:t>张三的项目简介</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p><w:p><w:bookmarkStart w:id="2" w:name="body1"/><w:r><w:t>这段正文应与下方图片同时出现。</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p><w:p><w:bookmarkStart w:id="3" w:name="photo0"/><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="照片"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="照片"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="image1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:bookmarkEnd w:id="3"/></w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1000" w:right="1000" w:bottom="1000" w:left="1000"/></w:sectPr></w:body></w:document>',
  );
  let mixedXml = await archive.file("word/document.xml")!.async("string");
  mixedXml = mixedXml
    .replace(
      "<w:body>",
      '<w:body><w:tbl><w:tblPr><w:tblW w:w="9600" w:type="dxa"/><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="6200"/><w:gridCol w:w="3400"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="6200" w:type="dxa"/></w:tcPr>',
    )
    .replace(
      '<w:p><w:bookmarkStart w:id="3"',
      '</w:tc><w:tc><w:tcPr><w:tcW w:w="3400" w:type="dxa"/></w:tcPr><w:p><w:bookmarkStart w:id="3"',
    )
    .replace("<w:sectPr>", "</w:tc></w:tr></w:tbl><w:sectPr>")
    .replaceAll('cx="914400" cy="914400"', 'cx="1828800" cy="1219200"');
  archive.file("word/document.xml", mixedXml);
  archive.file("word/media/photo.png", picture, { base64: true });
  const bytes = Array.from(await archive.generateAsync({ type: "uint8array" }));
  await page.goto("/");
  await page.evaluate((bytes) => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "office_preview")
        return {
          revision: 1,
          layout_available: true,
          reason: null,
          warnings: [],
          anchors: [
            {
              id: "body0",
              text: "张三的项目简介",
              display: { start: 0, end: 7 },
              label: "正文",
              available_in_layout: true,
            },
            {
              id: "body1",
              text: "这段正文应与下方图片同时出现。",
              display: { start: 8, end: 23 },
              label: "正文",
              available_in_layout: true,
            },
          ],
          images: [{ index: 0, name: "照片", occurrences: ["photo0"] }],
        };
      if (command === "office_preview_docx") return bytes;
      const value = await old(command, args);
      if (command === "analyze_file") {
        value.extension = "docx";
        value.meta.display_name = "图文文档.docx";
        value.text = "张三的项目简介\n这段正文应与下方图片同时出现。";
        value.entities[0].display = { start: 0, end: 2 };
        value.entities[0].effective_replacement = "某人";
        value.regions = [
          {
            ...value.regions[0],
            id: "image-manual",
            entity_id: null,
            source: "manual",
            replacement: "已脱敏",
          },
        ];
      }
      if (command === "document_manifest") value.kind = "docx";
      return value;
    };
  }, bytes);
  await page.getByRole("button", { name: "选择本机文件" }).click();
  const body = page.getByLabel("DOCX 排版内容");
  await expect(body).toBeVisible();
  await expect(body).toContainText("张三的项目简介");
  await expect(body.locator("img")).toBeVisible();
  await expect
    .poll(() =>
      body
        .locator("img")
        .evaluate((image: HTMLImageElement) => image.naturalWidth),
    )
    .toBe(180);
  await expect(body.locator("td")).toHaveCount(2);
  await body.getByRole("button", { name: "张三", exact: true }).click();
  await expect(page.locator(".entity-item.focused")).toContainText("张三");
  await page.screenshot({ path: testInfo.outputPath("mixed-word.png") });
  await page.getByRole("button", { name: "脱敏效果", exact: true }).click();
  await expect(body).toContainText("某人的项目简介");
  await expect
    .poll(() =>
      page.evaluate(() => window.__mock__.calls.document_draft_page ?? 0),
    )
    .toBeGreaterThan(0);
  await expect
    .poll(() =>
      body
        .locator("img")
        .evaluate((image: HTMLImageElement) => image.naturalWidth),
    )
    .toBe(1);
  await page.getByRole("button", { name: "原文标注", exact: true }).click();
  await expect(body).toContainText("张三的项目简介");
  await expect
    .poll(() =>
      body
        .locator("img")
        .evaluate((image: HTMLImageElement) => image.naturalWidth),
    )
    .toBe(180);
  await body.locator("img").click();
  await expect(page.getByAltText("内嵌图片 1")).toBeVisible();
  await page.getByRole("button", { name: "文档", exact: true }).click();
  await expect(body).toContainText("张三的项目简介");
  await expectNoHorizontalOverflow(page);
});

test("登录前可检查更新，确认后才下载并展示速度", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    window.__auth_status__ = {
      authenticated: false,
      offline: false,
      offline_until: null,
      subject: null,
      policy: null,
      reason: null,
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "检查更新" }).click();
  const dialog = page.getByRole("dialog", { name: "应用更新" });
  await expect(dialog.getByText("改进文档预览")).toBeVisible();
  expect(
    await page.evaluate(() => window.__mock__.calls.download_app_update ?? 0),
  ).toBe(0);
  await dialog.getByRole("button", { name: "下载更新" }).click();
  await expect(dialog.getByRole("progressbar")).toHaveAttribute(
    "value",
    "1048576",
  );
  await expect(dialog.getByText(/50%.*MB\/s/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: "取消下载" })).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("application-update.png"),
  });
  await dialog.getByRole("button", { name: "后台下载" }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByPlaceholder("请输入租户编码")).toBeVisible();
  await expectNoHorizontalOverflow(page);
});

test("租户用户登录后进入工作台", async ({ page }) => {
  await page.addInitScript(() => {
    window.__auth_status__ = {
      authenticated: false,
      offline: false,
      offline_until: null,
      subject: null,
      policy: null,
      reason: null,
    };
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "私匣" })).toBeVisible();
  const brandMark = page.locator(".login-brand .brand-mark");
  await expect(brandMark).toBeVisible();
  await expect
    .poll(() =>
      brandMark.evaluate((image: HTMLImageElement) => image.naturalWidth),
    )
    .toBeGreaterThan(0);
  await page.getByPlaceholder("请输入租户编码").fill("demo");
  await page.getByPlaceholder("请输入用户名").fill("tester");
  await page.getByPlaceholder("请输入密码").fill("password");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(page.getByRole("link", { name: "脱敏工作台" })).toBeVisible();
  await expect(page.locator(".brand .brand-mark")).toBeVisible();
  await expect(page.locator(".account-summary")).toContainText("测试用户");
  await expectNoHorizontalOverflow(page);
});

test("退出后清除工作台并返回登录页", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "退出登录" }).click();
  await expect(page.getByPlaceholder("请输入租户编码")).toBeVisible();
  await expect(page.getByRole("link", { name: "脱敏工作台" })).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.auth_logout))
    .toBe(1);
});

test("文件入口、视觉预览加载和侧栏折叠", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    window.__delayDocumentPreview__ = true;
  });
  await page.goto("/");
  await expect(page.getByLabel("待脱敏文本")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "自动识别文本" })).toHaveCount(
    0,
  );
  await expect(page.getByRole("heading", { name: "脱敏工作台" })).toHaveCount(
    0,
  );
  await expect(page.locator(".steps")).toHaveCount(0);

  await selectPdf(page);
  await expect(
    page.getByRole("status").filter({ hasText: "加载预览" }),
  ).toBeVisible();
  await expect(
    page
      .locator(".canvas-scroll")
      .getByText("OCR_TEXT_MUST_NOT_FLASH", { exact: false }),
  ).toHaveCount(0);
  await expect(page.locator(".app")).toHaveClass(/sidebar-collapsed/);

  await page.getByRole("button", { name: "展开侧边栏" }).click();
  await expect(page.locator(".app")).not.toHaveClass(/sidebar-collapsed/);
  await page.evaluate(() => window.__mock__.resolveDocumentPreview?.());
  await expect(page.getByAltText("第 1 页")).toBeVisible();
  await expect(
    page
      .locator(".canvas-scroll")
      .getByText("OCR_TEXT_MUST_NOT_FLASH", { exact: false }),
  ).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.document_manifest))
    .toBe(1);

  await page.getByRole("button", { name: "折叠侧边栏" }).click();
  await expect(page.locator(".app")).toHaveClass(/sidebar-collapsed/);
  await expect
    .poll(async () => (await page.locator(".app > aside").boundingBox())?.width)
    .toBeLessThanOrEqual(65);
  await expectNoHorizontalOverflow(page);
  await page.screenshot({
    path: testInfo.outputPath("workbench.png"),
    fullPage: false,
  });
});

test("框选即时预览、区域删除和撤销不等待 IPC", async ({ page }) => {
  await page.addInitScript(() => {
    window.__delayRegionMutations__ = true;
  });
  await page.goto("/");
  await selectPdf(page);

  await page.getByRole("button", { name: "框选", exact: true }).click();
  const canvas = page.getByTestId("region-canvas-page-0");
  const box = await canvas.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width * 0.2, box!.y + box!.height * 0.2);
  await page.mouse.down();
  await page.mouse.move(
    box!.x + box!.width * 0.45,
    box!.y + box!.height * 0.32,
    { steps: 4 },
  );
  await expect(page.getByTestId("region-draft")).toBeVisible();
  await page.mouse.up();

  await expect
    .poll(() => page.evaluate(() => window.__mock__.lastUpsert?.region?.id))
    .not.toBeFalsy();
  const regionId = await page.evaluate(
    () => window.__mock__.lastUpsert.region.id as string,
  );
  expect(
    await page.evaluate(() => window.__mock__.lastUpsert.expectedRevision),
  ).toBe(1);
  await expect(page.locator(`[data-region-id="${regionId}"]`)).toBeVisible();
  await expect(page.getByRole("tab", { name: /手动脱敏/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator(".save-state")).toContainText("正在保存");

  await page.evaluate(() => window.__mock__.resolveUpsert?.());
  await page.getByRole("button", { name: /删除区域/ }).click();
  await expect(page.locator(`[data-region-id="${regionId}"]`)).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() => window.__mock__.lastRemove?.expectedRevision),
    )
    .toBe(2);
  await expect(page.getByRole("button", { name: "撤销删除" })).toBeVisible();
  await page.getByRole("button", { name: "撤销删除" }).click();
  await expect(page.locator(`[data-region-id="${regionId}"]`)).toBeVisible();
  await page.evaluate(() => window.__mock__.resolveRemove?.());
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.upsert_region))
    .toBe(2);
  expect(
    await page.evaluate(() => window.__mock__.lastUpsert.expectedRevision),
  ).toBe(3);
  await page.evaluate(() => window.__mock__.resolveUpsert?.());
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.document_manifest))
    .toBe(1);
  await expectNoHorizontalOverflow(page);
});

test("检查器按任务动作自动打开并保持紧凑桌面布局", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  const entityTab = page.getByRole("tab", { name: /实体复核/ });
  const manualTab = page.getByRole("tab", { name: /手动脱敏/ });
  await expect(entityTab).toHaveAttribute("aria-selected", "true");
  await expect(page.getByLabel("选择张三")).toBeVisible();
  await manualTab.click();
  await expect(manualTab).toHaveAttribute("aria-selected", "true");
  await entityTab.click();
  await expect(entityTab).toHaveAttribute("aria-selected", "true");
  await expectNoHorizontalOverflow(page);
});

test("模型失败禁止选择文件", async ({ page }) => {
  await page.addInitScript(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (command: string, args: any) =>
      ["model_status", "load_models", "ensure_default_models"].includes(command)
        ? Promise.resolve({
            ready: false,
            error: "模型校验失败",
            location: "C:\\test",
            bytes: 0,
            version: null,
          })
        : old(command, args);
  });
  await page.goto("/");
  await expect(
    page.getByRole("button", { name: "选择本机文件" }),
  ).toBeDisabled();
  await page.getByRole("link", { name: "模型管理", exact: true }).click();
  await expect(page.getByText("模型校验失败")).toBeVisible();
  await expect(page.locator(".model-summary")).not.toHaveClass(/\bready\b/);
});

test("高级设置默认收起且恢复包要求两次口令一致", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByLabel("图片识别")).not.toBeVisible();
  await page.getByText("高级设置", { exact: true }).click();
  await expect(page.getByLabel("图片识别")).toBeVisible();
  await selectPdf(page);
  await page.getByRole("button", { name: /生成预览/ }).click();
  await page.getByText("创建可逆恢复包", { exact: true }).click();
  await page.getByLabel("恢复口令", { exact: true }).fill("password-one");
  await page.getByLabel("确认恢复口令").fill("password-two");
  await expect(page.getByText("两次输入的口令不一致")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "保存可逆恢复包" }),
  ).toBeDisabled();
  await page.getByLabel("确认恢复口令").fill("password-one");
  await expect(
    page.getByRole("button", { name: "保存可逆恢复包" }),
  ).toBeEnabled();
});

test("内置脱敏方式默认折叠，展开可查看并选择类型", async ({ page }) => {
  await page.goto("/#/policies");
  await expect(page.getByText("北京大学 → 某学校")).not.toBeVisible();
  await page.getByText("系统内置方式").click();
  await expect(page.getByText("北京大学 → 某学校")).toBeVisible();
  await page.getByRole("button", { name: "自定义" }).click();
  await expect(page.getByLabel("实体类型")).toHaveValue("ORGANIZATION");
});

test("AI 工具接入展示状态、复制配置并完成本机自检", async ({ page }) => {
  await page.goto("/#/integration");
  await expect(
    page.getByRole("heading", { name: "AI 工具接入" }),
  ).toBeVisible();
  await expect(page.getByText("默认开启", { exact: true })).toBeVisible();
  await expect(page.getByText("已登录", { exact: true })).toBeVisible();
  await expect(
    page.getByText("已随桌面应用安装", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("已安装，处理时按需加载", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("通用 MCP JSON 配置")).toContainText('"sixa"');
  await expect(
    page.getByText("只向 AI 返回任务状态与结果路径", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByText("检查状态 → 创建任务 → 等待终态 → 获取结果路径", {
      exact: true,
    }),
  ).toBeVisible();

  await page.getByRole("button", { name: "复制配置" }).click();
  await expect(page.getByRole("status")).toContainText("MCP 配置已复制");
  expect(await page.evaluate(() => window.__copied_text__)).toContain(
    "sixa-mcp.exe",
  );

  await page.getByRole("button", { name: "本机连接自检" }).click();
  await expect(page.getByRole("status")).toContainText("本机连接正常");
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.integration_check))
    .toBe(1);
  await expectNoHorizontalOverflow(page);
});

test("Mac MCP 配置保留中文空格路径并说明自动启动", async ({ page }) => {
  const path = "/Users/test/Applications/私匣 测试.app/Contents/MacOS/sixa-mcp";
  await page.addInitScript((value) => {
    window.__mcp_path__ = value;
  }, path);
  await page.goto("/#/integration");
  await expect(
    page.getByText("Mac 首次调用会自动打开私匣", { exact: false }),
  ).toBeVisible();
  await page.getByRole("button", { name: "复制配置" }).click();
  await expect
    .poll(() => page.evaluate(() => window.__copied_text__))
    .toBeTruthy();
  const config = JSON.parse(
    (await page.evaluate(() => window.__copied_text__))!,
  );
  expect(config.mcpServers.sixa).toEqual({ command: path, args: ["serve"] });
  await expectNoHorizontalOverflow(page);
});

test("重建模型需确认，取消时不删除模型", async ({ page }) => {
  await page.goto("/#/models");
  const rebuild = page.getByRole("button", { name: "重建模型" });
  page.once("dialog", (dialog) => dialog.dismiss());
  await rebuild.click();
  expect(await page.evaluate(() => window.__rebuild_count__ ?? 0)).toBe(0);
  page.once("dialog", (dialog) => dialog.accept());
  await rebuild.click();
  await expect
    .poll(() => page.evaluate(() => window.__rebuild_count__ ?? 0))
    .toBe(1);
});

test("模型下载展示进度、速度和剩余时间", async ({ page }) => {
  await page.goto("/#/models");
  await expect(page.getByRole("heading", { name: "模型管理" })).toBeVisible();
  await page.evaluate(() =>
    window.__mock__.emit("model-progress", {
      id: "raner-v1",
      stage: "downloading",
      current: 50 * 1024 * 1024,
      total: 100 * 1024 * 1024,
      percent: 50,
      bytes_per_second: 1024 * 1024,
      eta_seconds: 50,
      source: "https://example.invalid/raner-v1.zip",
      source_label: "阿里云 OSS",
      message: "正在从 阿里云 OSS 下载 中文实体识别模型",
    }),
  );
  const progress = page.locator(".download-progress");
  await expect(progress).toContainText(
    "正在从 阿里云 OSS 下载 中文实体识别模型",
  );
  await expect(progress).toContainText("当前来源：阿里云 OSS");
  await expect(progress).toContainText("50%");
  await expect(progress).toContainText("已下载 50 MB / 100 MB");
  await expect(progress).toContainText("下载速度 1.0 MB/秒");
  await expect(progress).toContainText("预计剩余 50 秒");
  await expect(page.getByRole("button", { name: "取消下载" })).toBeVisible();

  await page.evaluate(() =>
    window.__mock__.emit("model-progress", {
      id: "raner-v1",
      stage: "switching_source",
      current: 50 * 1024 * 1024,
      total: 100 * 1024 * 1024,
      percent: 50,
      source_label: "ModelScope 备用源",
      message: "阿里云 OSS 下载失败，正在切换到 ModelScope 备用源并继续下载",
    }),
  );
  await expect(progress).toContainText("正在切换到 ModelScope 备用源");
  await expect(progress).toContainText("当前来源：ModelScope 备用源");
});

test("基础模型加载状态不被可选高精度 OCR 覆盖", async ({ page }) => {
  await page.goto("/?delay-model-setup=1#/");
  const setup = page.locator(".setup-page");
  await expect(
    setup.getByRole("heading", { name: "正在准备本机识别能力" }),
  ).toBeVisible();

  await page.evaluate(() => {
    window.__mock__.emit("model-progress", {
      stage: "loading",
      percent: 100,
      message: "模型文件已就绪，正在加载到本机内存，首次加载可能需要几十秒",
    });
    window.__mock__.emit("model-progress", {
      id: "ppocrv4-accurate-v1",
      stage: "failed",
      message: "模型未就绪：未安装模型包",
    });
  });

  await expect(setup).toContainText("模型文件已就绪，正在加载到本机内存");
  await expect(setup).not.toContainText("未安装模型包");
});

test("筛选后的批量选择只影响可见实体", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    window.__mock__.task.entities.push({
      ...window.__mock__.task.entities[0],
      id: "phone",
      text: "13812345678",
      type_label: "手机号",
      entity_type: "PHONE",
    });
  });
  await selectPdf(page);
  await page.getByLabel("筛选实体").fill("手机号");
  await page.getByRole("button", { name: "取消选择", exact: true }).click();
  await expect(page.getByLabel("选择13812345678")).not.toBeChecked();
  await page.getByLabel("筛选实体").fill("");
  await expect(page.getByLabel("选择张三")).toBeChecked();
  await page.getByRole("link", { name: "任务历史" }).click();
  expect(
    await page.evaluate(
      () =>
        window.__mock__.task.entities.find((item: any) => item.id === "phone")
          .selected,
    ),
  ).toBe(false);
  expect(
    await page.evaluate(() => window.__mock__.task.entities[0].selected),
  ).toBe(true);
});

test("较早的保存响应不会覆盖新输入，离页会保存最新内容", async ({ page }) => {
  await page.addInitScript(() => {
    window.__delayReviewPatch__ = true;
  });
  await page.goto("/");
  await selectPdf(page);
  await page.locator(".entity-group-replacement summary").click();
  const input = page.getByLabel("张三全部替换文字");
  await input.fill("甲");
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.review_patch))
    .toBe(1);
  await input.fill("乙");
  await page.evaluate(() => window.__mock__.resolveReviewPatch?.());
  await expect(input).toHaveValue("乙");
  await page.getByRole("link", { name: "任务历史" }).click();
  await expect(page.getByRole("heading", { name: "任务历史" })).toBeVisible();
  expect(
    await page.evaluate(() => window.__mock__.task.entities[0].replacement),
  ).toBe("乙");
});

test("复核面板调整后不会遮住文档，布局偏好跨页面保留", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  const resize = page.getByRole("separator", { name: "调整复核面板宽度" });
  await resize.focus();
  await page.keyboard.press("ArrowLeft");
  const inspector = await page.getByLabel("复核检查器").boundingBox();
  const document = await page.getByLabel("文档预览").boundingBox();
  expect(inspector!.x).toBeGreaterThanOrEqual(document!.x + document!.width);
  expect(inspector!.width).toBeCloseTo(330, 0);
  await page.getByRole("button", { name: "展开侧边栏" }).click();
  await page.getByRole("link", { name: "应用设置" }).click();
  await expect(page.getByLabel("侧边栏", { exact: true })).toHaveValue(
    "expanded",
  );
  await page.getByRole("link", { name: "脱敏工作台" }).click();
  await expect(page.locator(".app")).not.toHaveClass(/sidebar-collapsed/);
});

test("批次无需修改即可确认检查，下一步返回正确批次", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "batch_view") {
        const task = window.__mock__.task;
        return {
          meta: { ...task.meta, id: "batch-a", kind: "batch" },
          items: [
            {
              index: 0,
              task_id: task.meta.id,
              state: task.meta.state,
              display_name: "测试简历.pdf",
              revision: task.revision,
              reviewed_revision: task.meta.reviewed_revision ?? 0,
            },
          ],
        };
      }
      return original(command, args);
    };
    location.hash = "/batch?id=batch-a";
  });
  await expect(page.getByText("待检查 1", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "打开检查" }).click();
  await page.getByRole("button", { name: "检查完成，返回批次" }).click();
  await expect(page.getByText("待检查 0", { exact: true })).toBeVisible();
  await expect(page.getByText("已确认检查", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => window.__mock__.calls.confirm_review)).toBe(
    1,
  );
  expect(
    await page.evaluate(() => window.__mock__.calls.review_patch ?? 0),
  ).toBe(0);
});

test("批次进度按编号隔离，离页返回仍可查看并取消", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "batch_view")
        return {
          meta: {
            ...window.__mock__.task.meta,
            id: "batch-a",
            kind: "batch",
            state: "analyzing",
          },
          items: [],
        };
      return original(command, args);
    };
    location.hash = "/batch?id=batch-a";
  });
  await page.getByRole("heading", { name: "批量处理" }).waitFor();
  await page.evaluate(() => {
    window.__mock__.emit("batch-progress", {
      id: "batch-a",
      stage: "analyzing",
      done: 1,
      total: 3,
    });
    window.__mock__.emit("batch-progress", {
      id: "batch-b",
      stage: "processing",
      done: 99,
      total: 100,
    });
  });
  await expect(page.locator(".inline-progress")).toContainText("1 / 3");
  await expect(page.locator(".inline-progress")).not.toContainText("99");
  await page.getByRole("link", { name: "任务历史" }).click();
  await page.evaluate(() => {
    location.hash = "/batch?id=batch-a";
  });
  await expect(page.locator(".inline-progress")).toContainText("1 / 3");
  await expect(
    page.getByRole("button", { name: "取消剩余任务" }),
  ).toBeEnabled();
});

test("多页预览按需读取二进制图片，最多两个并行请求且返回时复用缓存", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    const stats = { active: 0, peak: 0, pages: [] as number[] };
    (window as any).__pageStats = stats;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "document_manifest")
        return {
          revision: 1,
          pages: Array.from({ length: 20 }, (_, index) => ({
            index,
            width: 600,
            height: 840,
            preview_uri: "",
          })),
          text: null,
          warnings: [],
        };
      if (command === "document_page") {
        stats.active++;
        stats.peak = Math.max(stats.peak, stats.active);
        stats.pages.push(args.page);
        await new Promise((resolve) => setTimeout(resolve, 80));
        stats.active--;
        return Uint8Array.from(
          atob(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a6XcAAAAASUVORK5CYII=",
          ),
          (c) => c.charCodeAt(0),
        ).buffer;
      }
      return original(command, args);
    };
  });
  await selectPdf(page);
  await expect(page.getByAltText("第 1 页", { exact: true })).toBeVisible();
  expect(
    await page.evaluate(() => (window as any).__pageStats.pages.length),
  ).toBeLessThan(20);
  await page.getByLabel("跳转页码").fill("20");
  await page.getByLabel("跳转页码").press("Enter");
  await expect(page.getByAltText("第 20 页", { exact: true })).toBeVisible();
  await page.getByLabel("跳转页码").fill("1");
  await page.getByLabel("跳转页码").press("Enter");
  await expect(page.getByAltText("第 1 页", { exact: true })).toBeVisible();
  expect(
    await page.evaluate(() => (window as any).__pageStats.peak),
  ).toBeLessThanOrEqual(2);
  expect(
    await page.evaluate(
      () =>
        (window as any).__pageStats.pages.filter((p: number) => p === 0).length,
    ),
  ).toBe(1);
  await page.locator(".canvas-scroll").evaluate((element) => {
    const fifth = element.querySelector('[data-page="4"]')!;
    element.scrollTop +=
      fifth.getBoundingClientRect().top - element.getBoundingClientRect().top;
  });
  await expect(page.getByLabel("跳转页码")).toHaveValue("5");
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(page.getByLabel("跳转页码")).toHaveValue("6");
});

test("手工区域支持移动、调整大小与修改替换文字", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await page.getByRole("button", { name: "框选", exact: true }).click();
  const canvas = page.getByTestId("region-canvas-page-0");
  const bounds = (await canvas.boundingBox())!;
  await page.mouse.move(
    bounds.x + bounds.width * 0.3,
    bounds.y + bounds.height * 0.3,
  );
  await page.mouse.down();
  await page.mouse.move(
    bounds.x + bounds.width * 0.5,
    bounds.y + bounds.height * 0.4,
  );
  await page.mouse.up();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.upsert_region))
    .toBe(1);
  const original = await page.evaluate(
    () => window.__mock__.lastUpsert.region.polygon,
  );
  await page.getByRole("button", { name: "完成框选", exact: true }).click();
  const region = page.locator("polygon.manual");
  const box = (await region.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(
    box.x + box.width / 2 + 20,
    box.y + box.height / 2 + 15,
  );
  await page.mouse.up();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.upsert_region))
    .toBe(2);
  expect(
    await page.evaluate(() => window.__mock__.lastUpsert.region.polygon[0].x),
  ).toBeGreaterThan(original[0].x);
  const handle = (await page.locator(".region-handle").boundingBox())!;
  await page.mouse.move(
    handle.x + handle.width / 2,
    handle.y + handle.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    handle.x + handle.width / 2 + 20,
    handle.y + handle.height / 2 + 10,
  );
  await page.mouse.up();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.upsert_region))
    .toBe(3);
  await page.getByLabel("第 1 页区域替换文字").fill("隐去内容");
  await expect
    .poll(() =>
      page.evaluate(() => window.__mock__.lastUpsert.region.replacement),
    )
    .toBe("隐去内容");
  await expect(page.locator("polygon.manual.focused")).toBeVisible();
  await page.getByRole("button", { name: "脱敏效果", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(() => window.__mock__.calls.document_draft_page ?? 0),
    )
    .toBeGreaterThan(0);
  await expect(page.locator(".redaction-preview")).toHaveCount(0);
});

test("已安装但空闲卸载的模型仍允许单文件和批量任务按需启动", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() =>
    window.__mock__.emit("model-progress", {
      ready: false,
      error: null,
      capabilities: [
        { id: "raner-v1", installed: true, ready: false, state: "installed" },
        {
          id: "ppocrv4-mobile-v1",
          installed: true,
          ready: false,
          state: "installed",
        },
      ],
    }),
  );
  await expect(
    page.getByRole("button", { name: "选择本机文件" }),
  ).toBeEnabled();
  await selectPdf(page);
  await page.getByRole("link", { name: "批量处理" }).click();
  await expect(
    page.getByRole("button", { name: "选择多个文件" }),
  ).toBeEnabled();
});

test("批次重试使用新编号并保留旧批次，确认状态遵循服务端字段", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    const view = (id: string, state: string) => ({
      meta: { ...window.__mock__.task.meta, id, kind: "batch", state },
      items: [
        {
          index: 0,
          task_id: "task-pdf-1",
          state: id === "old-batch" ? "failed" : "awaiting_review",
          reviewed_revision: 1,
          review_confirmed: false,
          display_name: id === "old-batch" ? "旧任务.pdf" : "新任务.pdf",
        },
      ],
    });
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "batch_view")
        return view(
          args.id,
          args.id === "old-batch" ? "failed" : "awaiting_review",
        );
      if (command === "retry_batch") {
        (window as any).__retryArgs = args;
        window.__mock__.emit("batch-progress", {
          id: args.requestId,
          stage: "analyzing",
          done: 0,
          total: 1,
        });
        return new Promise((resolve) => {
          (window as any).__finishRetry = () =>
            resolve(view(args.requestId, "awaiting_review"));
        });
      }
      return original(command, args);
    };
    location.hash = "/batch?id=old-batch";
  });
  await page.getByRole("button", { name: "重试失败项" }).click();
  const request = await page.evaluate(() => (window as any).__retryArgs);
  expect(request.id).toBe("old-batch");
  expect(request.requestId).not.toBe("old-batch");
  await expect(page).toHaveURL(new RegExp(request.requestId));
  await expect(
    page.getByRole("button", { name: "取消剩余任务" }),
  ).toBeVisible();
  await page.evaluate(() => (window as any).__finishRetry());
  await expect(page.getByText("新任务.pdf", { exact: true })).toBeVisible();
  await expect(page.getByText("待检查 1", { exact: true })).toBeVisible();
  await expect(page.getByText("已确认检查", { exact: true })).toHaveCount(0);
  await page.evaluate(() => {
    location.hash = "/batch?id=old-batch";
  });
  await expect(page.getByText("旧任务.pdf", { exact: true })).toBeVisible();
});

test("关闭窗口的询问在登录前可用，后台运行不会卸载当前界面", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__auth_status__ = {
      authenticated: false,
      subject: null,
      reason: null,
      offline: false,
    };
  });
  await page.goto("/");
  await expect(page.getByPlaceholder("请输入租户编码")).toBeVisible();
  await page.evaluate(() =>
    window.__mock__.emit("app-close-request", {
      id: "before-login",
      phase: "choice",
      tray_available: true,
      active_tasks: 0,
      active_downloads: 0,
    }),
  );
  const closeDialog = page.getByRole("dialog", { name: "关闭私匣" });
  await expect(closeDialog).toBeVisible();
  await expect(closeDialog.getByRole("checkbox")).not.toBeChecked();
  await closeDialog.getByRole("button", { name: "最小化到托盘" }).click();
  await expect(closeDialog).toHaveCount(0);
  await expect(page.getByPlaceholder("请输入租户编码")).toBeVisible();
  expect(await page.evaluate(() => window.__mock__.closeReplies)).toEqual([
    { requestId: "before-login", action: "tray", remember: false },
  ]);
});

test("退出先保存修改，再确认停止任务并处理慢退出", async ({ page }) => {
  await page.addInitScript(() => {
    window.__delayReviewPatch__ = true;
  });
  await page.goto("/");
  await selectPdf(page);
  await page.locator(".entity-group-replacement summary").click();
  await page.getByLabel("张三全部替换文字").fill("保存后退出");
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.review_patch))
    .toBe(1);
  await page.evaluate(() =>
    window.__mock__.emit("app-close-request", {
      id: "active-close",
      phase: "choice",
      tray_available: true,
      active_tasks: 2,
      active_downloads: 1,
    }),
  );
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "直接退出" })
    .click();
  await expect(
    page.getByRole("dialog", { name: "正在保存修改" }),
  ).toBeVisible();
  expect(
    await page.evaluate(() =>
      window.__mock__.closeReplies.some(
        (reply: any) => reply.action === "saved",
      ),
    ),
  ).toBe(false);
  await page.evaluate(() => window.__mock__.resolveReviewPatch?.());
  await expect(
    page.getByRole("dialog", { name: "仍有任务正在运行" }),
  ).toContainText("2 个处理任务、1 个下载");
  await page.getByRole("button", { name: "停止并退出" }).click();
  await expect(
    page.getByRole("dialog", { name: "正在停止任务" }),
  ).toBeVisible();
  await page.evaluate(() =>
    window.__mock__.emit("app-close-request", {
      id: "active-close",
      phase: "slow",
      tray_available: true,
      active_tasks: 1,
      active_downloads: 0,
    }),
  );
  await expect(page.getByRole("dialog")).toContainText(
    "可能丢失尚未保存的修改",
  );
  await page.getByRole("button", { name: "继续等待" }).click();
  await expect(
    page.getByRole("dialog", { name: "正在停止任务" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "取消退出" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      window.__mock__.closeReplies.map((reply: any) => reply.action),
    ),
  ).toEqual(["exit", "saved", "stop", "wait", "cancel"]);
  expect(
    await page.evaluate(() => window.__mock__.task.entities[0].replacement),
  ).toBe("保存后退出");
});

test("托盘打开设置在未登录时保留目标，关闭偏好仅在后端成功后更新", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__auth_status__ = {
      authenticated: false,
      subject: null,
      reason: null,
      offline: false,
    };
  });
  await page.goto("/");
  await expect(page.getByPlaceholder("请输入租户编码")).toBeVisible();
  await page.evaluate(() => window.__mock__.emit("app-open-settings", null));
  await expect(page).toHaveURL(/#\/settings$/);
  await page.getByPlaceholder("请输入租户编码").fill("demo");
  await page.getByPlaceholder("请输入用户名").fill("tester");
  await page.getByPlaceholder("请输入密码").fill("password");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(page.getByRole("heading", { name: "应用设置" })).toBeVisible();
  const setting = page.getByLabel("关闭窗口时");
  await expect(setting).toHaveValue("ask");
  await setting.selectOption("tray");
  await expect(setting).toHaveValue("tray");
  await page.evaluate(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "set_desktop_preferences") throw Error("偏好保存失败");
      return original(command, args);
    };
  });
  await setting.selectOption("exit");
  await expect(page.getByRole("alert")).toContainText("偏好保存失败");
  await expect(setting).toHaveValue("tray");
});

test("保存超时可以强制退出，但运行中任务仍需确认停止", async ({ page }) => {
  await page.addInitScript(() => {
    window.__delayReviewPatch__ = true;
  });
  await page.goto("/");
  await selectPdf(page);
  await page.clock.install();
  await page.locator(".entity-group-replacement summary").click();
  await page.getByLabel("张三全部替换文字").fill("尚未确认保存");
  await page.evaluate(() =>
    window.__mock__.emit("app-close-request", {
      id: "force-save",
      phase: "saving",
      tray_available: true,
      active_tasks: 1,
      active_downloads: 0,
    }),
  );
  await expect(
    page.getByRole("dialog", { name: "正在保存修改" }),
  ).toBeVisible();
  await page.clock.fastForward(5100);
  await expect(page.getByRole("dialog")).toContainText(
    "可能丢失尚未保存的修改",
  );
  await page.getByRole("button", { name: "强制退出" }).click();
  await expect(
    page.getByRole("dialog", { name: "仍有任务正在运行" }),
  ).toBeVisible();
  expect(
    await page.evaluate(() =>
      window.__mock__.closeReplies.map((reply: any) => reply.action),
    ),
  ).toEqual(["force"]);
  await page.getByRole("button", { name: "取消退出" }).click();
  await page.evaluate(() => window.__mock__.resolveReviewPatch?.());
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      window.__mock__.closeReplies.some(
        (reply: any) => reply.action === "saved",
      ),
    ),
  ).toBe(false);
});

test("文件管理展示默认目录，筛选与空状态在桌面尺寸可用", async ({ page }) => {
  await page.goto("/#/files");
  await expect(
    page.getByRole("heading", { name: "文件管理", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("C:\\test\\exports", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("当前文件夹没有符合条件的文件。")).toBeVisible();
  await page.getByLabel("搜索处理结果").fill("合同");
  await expect(page.getByText("当前文件夹没有符合条件的文件。")).toBeVisible();
  await page.getByLabel("筛选文件格式").selectOption("pdf");
  await page.getByRole("button", { name: "更改位置" }).click();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.set_file_directory))
    .toBe(1);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("离开已生成但未保存的任务前提供选择，并保留历史记录", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  await page.getByRole("button", { name: "生成预览", exact: true }).click();
  await expect(page.getByText("预览已生成，尚未保存到文件夹。")).toBeVisible();
  await page.getByRole("link", { name: "文件管理" }).click();
  const dialog = page.getByRole("dialog", { name: "结果尚未保存" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "暂不保存并离开" }).click();
  await expect(page.getByRole("heading", { name: "文件管理" })).toBeVisible();
  expect(await page.evaluate(() => window.__mock__.task.meta.state)).toBe(
    "completed",
  );
});

test("Mac 接入程序缺失时不提供无效配置，并提供明确修复方法", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "integration_info")
        return {
          enabled: true,
          mcp_available: false,
          authenticated: true,
          models_ready: true,
          app_version: "1.0.10",
          platform: "macos",
          executable_path: "/Applications/私匣.app/Contents/MacOS/sixa-mcp",
          protocol_version: "1",
          supported_formats: ["PDF", "DOCX"],
        };
      return original(command, args);
    };
  });
  await page.goto("/#/integration");
  await expect(page.getByRole("alert")).toContainText("完整的私匣应用");
  await expect(
    page.getByRole("button", { name: "复制配置", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "重新检测安装" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "复制接入诊断信息" }).click();
  expect(await page.evaluate(() => window.__copied_text__)).toContain(
    '"platform": "macos"',
  );
});

test("有处理结果时所有文件操作均留在可见表格内", async ({ page }) => {
  await page.addInitScript(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "output_task_view")
        return {
          ...window.__mock__.task,
          meta: { ...window.__mock__.task.meta, state: "completed" },
        };
      if (command === "browse_directory")
        return {
          root: "C:/Users/用户/Documents/私匣/处理结果",
          path: "C:/Users/用户/Documents/私匣/处理结果",
          relative: "",
          total: 1,
          items: [
            {
              directory: false,
              modified: 1793000000,
              output: {
                id: "saved-1",
                task_id: "task-pdf-1",
                task_exists: true,
                kind: "pdf",
                source: "manual",
              },
              id: "saved-1",
              task_id: "task-pdf-1",
              name: "项目合作协议_脱敏.pdf",
              kind: "pdf",
              path: "C:/Users/用户/Documents/私匣/处理结果/项目合作协议_脱敏.pdf",
              created_at: 1793000000,
              size: 192032,
              error: null,
              generation: "1",
              partial: false,
              failed_count: 0,
              source: "automatic",
              exists: true,
              task_exists: true,
            },
          ],
        };
      return original(command, args);
    };
  });
  await page.goto("/#/files");
  await expect(
    page.getByText("项目合作协议_脱敏.pdf", { exact: true }),
  ).toBeVisible();
  const bounds = await page.locator(".table-scroll").boundingBox();
  for (const label of ["查看任务", "打开文件", "所在文件夹", "另存为"]) {
    const button = page.getByRole("button", { name: label, exact: true });
    await expect(button).toBeVisible();
    const box = await button.boundingBox();
    expect(box!.x + box!.width).toBeLessThanOrEqual(
      bounds!.x + bounds!.width + 1,
    );
  }
  await page.getByRole("button", { name: "查看任务", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "已生成文件", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
});

test("@workflow 同词合并且仍可逐处选择", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    window.__mock__.task.entities.push({
      ...window.__mock__.task.entities[0],
      id: "repeat",
      selected: false,
    });
  });
  await selectPdf(page);
  await expect(page.locator(".entity-group")).toHaveCount(1);
  await expect(page.locator(".entity-group-heading")).toContainText("共 2 处");
  expect(
    await page
      .getByLabel("选择张三", { exact: true })
      .evaluate((el: HTMLInputElement) => el.indeterminate),
  ).toBe(true);
  await page.getByLabel("选择张三", { exact: true }).check();
  await page.locator(".entity-group-heading button").click();
  await expect(page.getByLabel("第2处张三的选择状态")).toBeChecked();
  await page.getByLabel("第2处张三的选择状态").uncheck();
  expect(
    await page
      .getByLabel("选择张三", { exact: true })
      .evaluate((el: HTMLInputElement) => el.indeterminate),
  ).toBe(true);
});

test("@workflow 保存选择子目录并返回文件管理，取消不导出", async ({ page }) => {
  await page.addInitScript(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "browse_directory")
        return {
          root: "C:/结果",
          path: `C:/结果/${args.query.relative || ""}`,
          relative: args.query.relative || "",
          items: args.query.relative
            ? []
            : [
                {
                  name: "项目甲",
                  path: "C:/结果/项目甲",
                  directory: true,
                  size: 0,
                  modified: 0,
                  output: null,
                },
              ],
          total: args.query.relative ? 0 : 1,
        };
      return old(command, args);
    };
  });
  await page.goto("/");
  await selectPdf(page);
  await page.getByRole("button", { name: "生成预览", exact: true }).click();
  await page.getByRole("button", { name: "保存到文件夹", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "保存脱敏文件" });
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  expect(
    await page.evaluate(() => window.__mock__.calls.save_result || 0),
  ).toBe(0);
  await page.getByRole("button", { name: "保存到文件夹", exact: true }).click();
  await dialog.getByRole("button", { name: "项目甲", exact: true }).click();
  await expect(dialog).toContainText("C:/结果/项目甲");
  await dialog.getByRole("button", { name: "保存到当前文件夹" }).click();
  await expect(page).toHaveURL(/files\?dir=/);
  await expect(
    page.getByRole("heading", { name: "文件管理", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => window.__mock__.calls.save_result)).toBe(1);
});

test("@workflow Word 内容预览同步双向滚动和选词", async ({ page }) => {
  await page.addInitScript(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "office_preview")
        return {
          revision: 1,
          layout_available: false,
          reason: "文档采用内容预览",
          warnings: [],
          images: [],
          anchors: Array.from({ length: 500 }, (_, i) => ({
            id: `sixa_text_body_${i}`,
            text: args.result
              ? `段落${i} 某公司公开资料`
              : `段落${i} 青岛市企业公开宣传资料，文字有一定长度。`.repeat(3),
            display: { start: i * 100, end: i * 100 + 20 },
            label: "正文",
            available_in_layout: false,
          })),
        };
      const result = await old(command, args);
      if (command === "analyze_file") {
        result.extension = "docx";
        result.meta.kind = "docx";
        result.meta.state = "completed";
        result.meta.display_name = "测试.docx";
      }
      if (command === "document_manifest") result.kind = "docx";
      return result;
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "选择本机文件" }).click();
  await page.getByRole("button", { name: "对比", exact: true }).click();
  const source = page
    .locator(".preview-pane")
    .first()
    .getByLabel("完整分块内容");
  const result = page
    .locator(".preview-pane")
    .last()
    .getByLabel("完整分块内容");
  await expect(source).toBeVisible();
  await expect(result).toBeVisible();
  const visible = (el: HTMLElement) => {
    const top = el.getBoundingClientRect().top;
    return [...el.querySelectorAll<HTMLElement>("[data-content-anchor]")].find(
      (r) => r.getBoundingClientRect().bottom > top + 2,
    )?.dataset.contentAnchor;
  };
  await source.evaluate((el) => {
    el.scrollTop = 2300;
  });
  await expect
    .poll(async () => {
      const a = await source.evaluate(visible);
      return !!a && a === (await result.evaluate(visible));
    })
    .toBe(true);
  await result.evaluate((el) => {
    el.scrollTop = 4400;
  });
  await expect
    .poll(async () => {
      const a = await result.evaluate(visible);
      return !!a && a === (await source.evaluate(visible));
    })
    .toBe(true);
  await page.getByLabel("同步滚动").uncheck();
  const before = await result.evaluate((el) => el.scrollTop);
  await source.evaluate((el) => {
    el.scrollTop = 800;
  });
  await page.waitForTimeout(150);
  expect(await result.evaluate((el) => el.scrollTop)).toBe(before);
  expect(await page.locator(".sixa-docx-content-row").count()).toBeLessThan(60);
});

test("@workflow Word 选词快速替换、保存规则及撤销无需切页", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const old = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
      if (command === "office_preview")
        return {
          revision: window.__mock__.task.revision,
          layout_available: false,
          reason: "内容预览",
          warnings: [],
          images: [],
          anchors: [
            {
              id: "word_0",
              text: "青岛市",
              display: { start: 0, end: 3 },
              label: "正文",
              available_in_layout: false,
            },
          ],
        };
      if (command === "find_text")
        return {
          revision: window.__mock__.task.revision,
          total: 1,
          items: [
            {
              id: "match",
              display: { start: 0, end: 3 },
              context: "青岛市",
              conflicts: 0,
              conflict_details: [],
              locatable: true,
            },
          ],
        };
      if (command === "apply_text") {
        window.__mock__.calls.apply_text =
          (window.__mock__.calls.apply_text || 0) + 1;
        const task = window.__mock__.task;
        task.revision++;
        task.entities = [
          {
            id: "manual",
            entity_type: "手动文字",
            type_label: "手动文字",
            text: "青岛市",
            selected: true,
            replacement: args.request.replacement,
            source: "手动添加",
            score: 1,
            display: { start: 0, end: 3 },
          },
        ];
        return task;
      }
      if (command === "save_quick_rule") {
        window.__mock__.calls.save_quick_rule = 1;
        return;
      }
      if (command === "undo_text") {
        window.__mock__.calls.undo_text = 1;
        const task = window.__mock__.task;
        task.revision++;
        task.entities = [];
        return task;
      }
      const value = await old(command, args);
      if (command === "analyze_file") {
        value.extension = "docx";
        value.meta.kind = "docx";
        value.text = "青岛市";
        value.entities = [];
        value.regions = [];
        window.__mock__.task = value;
      }
      if (command === "document_manifest") value.kind = "docx";
      return value;
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "选择本机文件" }).click();
  const text = page.locator(".sixa-docx-content-row > div").first();
  await expect(text).toHaveText("青岛市");
  await text.evaluate((el) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  await expect(page.getByLabel("查找脱敏文字")).toHaveValue("青岛市");
  await page.getByLabel("手动替换文字").fill("某地");
  await page.getByLabel("同时保存为规则，供以后使用").check();
  await page.getByLabel("快捷规则名称").fill("客户地点");
  await page.getByRole("button", { name: "查找", exact: true }).click();
  await expect(page.getByText("当前文件共命中 1 处")).toBeVisible();
  await page.getByRole("button", { name: "全部替换", exact: true }).click();
  await expect(page.getByText("已替换 1 处并保存规则。")).toBeVisible();
  expect(await page.evaluate(() => window.__mock__.calls.apply_text)).toBe(1);
  expect(await page.evaluate(() => window.__mock__.calls.save_quick_rule)).toBe(
    1,
  );
  await page.getByRole("button", { name: "撤销", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.undo_text))
    .toBe(1);
  await expect(page).toHaveURL(/\/$/);
});
