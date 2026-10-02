import { expect, startDaemon, test } from "./harness";

// covers: M7-AC-02
test("a failed download says so and can be retried", async ({ page, daemon }) => {
  await daemon.openSystem(page);
  await page.getByText("For experts: enter a model address").click();
  await page.getByRole("textbox", { name: "Model", exact: true }).fill("demo/Broken-GGUF");
  await expect(page.getByText(/fits ✓/)).toBeVisible();
  await page.getByRole("button", { name: "Add", exact: true }).click();
  const line = page.getByTestId("model-broken-q8_0");
  await expect(line.getByRole("alert")).toContainText("download failed", { timeout: 20_000 });
  await line.getByRole("button", { name: "Try again" }).click();
  await expect(line.getByTestId("model-status")).toHaveText(/downloading|download failed/);
});

test.describe("on a machine that is too small", () => {
  test.use({ daemonArgs: ["--ram-gib", "1"] });
  test("a model that does not fit is explained and cannot be added", async ({ page, daemon }) => {
    await daemon.open(page);
    // The guided choice says so honestly …
    await page.getByRole("button", { name: "Next" }).click();
    await expect(page.getByTestId("step-model")).toContainText("none of Ancilo's models fits comfortably on this computer (1.0 GB memory)");
    // … and so does the expert way.
    await daemon.openSystem(page);
    await page.getByText("For experts: enter a model address").click();
    await page.getByRole("textbox", { name: "Model", exact: true }).fill("demo/Chat-GGUF");
    await expect(page.getByText(/does not fit/)).toBeVisible();
    await expect(page.getByRole("button", { name: "Add", exact: true })).toBeDisabled();
  });
});

test.describe("when Claude Code is not logged in", () => {
  test.use({ daemonArgs: ["--claude-fails", "--with-models"] });
  test("connecting fails with the reason", async ({ page, daemon }) => {
    await daemon.openSystem(page, false);
    await page.getByRole("button", { name: "Connect Claude Code" }).click();
    await expect(page.getByRole("alert")).toContainText("Connecting Claude Code failed");
    await expect(page.getByRole("alert")).toContainText("not logged in");
  });
});

test("when the daemon goes away, the app says so", async ({ page }) => {
  const d = await startDaemon();
  await d.open(page);
  d.kill();
  await expect(page.getByTestId("offline")).toContainText("Ancilo is not running", { timeout: 15_000 });
  await expect(page.getByTestId("offline")).toContainText("ancilo daemon start");
});
