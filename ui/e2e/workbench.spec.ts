import { expect, test, type Page } from "@playwright/test";

declare global {
  interface Window {
    __mock__: {
      calls: Record<string, number>;
      task: any;
      lastUpsert: any;
      lastRemove: any;
      resolveDocumentPreview?: () => void;
      resolveUpsert?: () => void;
      resolveRemove?: () => void;
    };
    __delayDocumentPreview__?: boolean;
    __delayRegionMutations__?: boolean;
    __rebuild_count__?: number;
    __auth_status__?: any;
    __copied_text__?: string;
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
    const state = (window.__mock__ = {
      calls: {},
      task,
      lastUpsert: null,
      lastRemove: null,
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
      model_status: () => ({
        ready: true,
        version: "test-only",
        location: "C:\\test\\models",
        bytes: 100,
        error: null,
        capabilities: [
          {
            id: "raner-v1",
            label: "中文实体识别",
            installed: true,
            ready: true,
            version: "test-only",
            bytes: 50,
            location: "C:\\test\\models\\raner-v1",
            error: null,
          },
          {
            id: "ppocrv4-mobile-v1",
            label: "轻量 OCR",
            installed: true,
            ready: true,
            version: "test-only",
            bytes: 50,
            location: "C:\\test\\models\\ppocrv4-mobile-v1",
            error: null,
          },
        ],
      }),
      load_models: () => handlers.model_status({}),
      ensure_default_models: () => handlers.model_status({}),
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
      select_entities: ({ selections }) => {
        task.entities = task.entities.map((entity: any) => ({
          ...entity,
          ...selections.find((item: any) => item.id === entity.id),
        }));
        task.revision += 1;
        state.task = task;
        return task;
      },
      preview: () => "OCR_TEXT_MUST_NOT_FLASH 某人在北京",
      document_preview: () => {
        if (!window.__delayDocumentPreview__) return documentPreview;
        return new Promise((resolve) => {
          state.resolveDocumentPreview = () => resolve(documentPreview);
        });
      },
      document_result_preview: () => documentPreview,
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
      export_recovery: () => undefined,
      list_tasks: () => [task.meta],
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
        if (command === "plugin:event|listen") return 1;
        if (command === "plugin:event|unlisten") return undefined;
        const handler = handlers[command];
        if (!handler) throw Error("未模拟命令 " + command);
        return structuredClone(await handler(args));
      },
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { label: "main" },
      },
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
  await expect(
    page.getByRole("heading", { name: "私匣" }),
  ).toBeVisible();
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
    page.getByText("OCR_TEXT_MUST_NOT_FLASH", { exact: false }),
  ).toHaveCount(0);
  await expect(page.locator(".app")).toHaveClass(/sidebar-collapsed/);

  await page.getByRole("button", { name: "展开侧边栏" }).click();
  await expect(page.locator(".app")).not.toHaveClass(/sidebar-collapsed/);
  await page.evaluate(() => window.__mock__.resolveDocumentPreview?.());
  await expect(page.getByAltText("第 1 页")).toBeVisible();
  await expect(
    page.getByText("OCR_TEXT_MUST_NOT_FLASH", { exact: false }),
  ).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => window.__mock__.calls.document_preview))
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

  const canvas = page.getByTestId("region-canvas-page-0");
  const box = await canvas.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + 80, box!.y + 110);
  await page.mouse.down();
  await page.mouse.move(box!.x + 210, box!.y + 190, { steps: 4 });
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
  await expect(page.getByRole("tab", { name: /手工区域/ })).toHaveAttribute(
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
    .poll(() => page.evaluate(() => window.__mock__.calls.document_preview))
    .toBe(1);
  await expectNoHorizontalOverflow(page);
});

test("检查器按任务动作自动打开并保持紧凑桌面布局", async ({ page }) => {
  await page.goto("/");
  await selectPdf(page);
  const entityTab = page.getByRole("tab", { name: /实体复核/ });
  const manualTab = page.getByRole("tab", { name: /手工区域/ });
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
  await page.getByRole("button", { name: /生成脱敏文件/ }).click();
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
  await expect(page.getByRole("heading", { name: "AI 工具接入" })).toBeVisible();
  await expect(page.getByText("默认开启", { exact: true })).toBeVisible();
  await expect(page.getByText("已登录，可接受任务", { exact: true })).toBeVisible();
  await expect(page.getByText("已随桌面应用安装", { exact: true })).toBeVisible();
  await expect(page.getByText("已就绪", { exact: true })).toBeVisible();
  await expect(page.getByLabel("通用 MCP JSON 配置")).toContainText(
    '"sixa"',
  );
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
