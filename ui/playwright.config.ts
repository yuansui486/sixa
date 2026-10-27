import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  use: {
    channel: process.env.PLAYWRIGHT_CHANNEL || undefined,
    baseURL: "http://127.0.0.1:1420",
    viewport: { width: 1366, height: 900 },
    locale: "zh-CN",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: false,
  },
  projects: [
    ...(process.env.SIXA_TEST_WEBKIT
      ? [
          {
            name: "webkit-desktop",
            grep: /@workflow/,
            use: { browserName: "webkit" as const, channel: undefined },
          },
        ]
      : []),
    { name: "desktop", use: { browserName: "chromium" } },
    {
      name: "compact-desktop",
      use: { browserName: "chromium", viewport: { width: 960, height: 640 } },
    },
  ],
});
