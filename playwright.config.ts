import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  outputDir: "evidence/playwright/results",
  reporter: [
    ["list"],
    ["html", { outputFolder: "evidence/playwright/report", open: "never" }],
  ],
  use: {
    baseURL: "http://127.0.0.1:1422",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "desktop-dark",
      use: {
        browserName: "chromium",
        viewport: { width: 1440, height: 900 },
        colorScheme: "dark",
      },
    },
    {
      name: "desktop-light",
      use: {
        browserName: "chromium",
        viewport: { width: 1000, height: 1100 },
        colorScheme: "light",
      },
    },
    {
      name: "narrow-dark",
      use: {
        browserName: "chromium",
        viewport: { width: 390, height: 844 },
        colorScheme: "dark",
      },
    },
  ],
  webServer: {
    command: "bun run build && bun tests/e2e/serve.ts",
    url: "http://127.0.0.1:1422",
    reuseExistingServer: false,
  },
});
