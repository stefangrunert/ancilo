import { defineConfig, devices } from "@playwright/test";

// Every test starts its own daemon (the `ancilo-e2e` harness with fake
// Hugging Face, fake models and fake Claude Code/Codex) – see e2e/harness.ts.
export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  workers: 4,
  reporter: [["list"], ["junit", { outputFile: "test-results/junit.xml" }]],
  use: { trace: "retain-on-failure" },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
