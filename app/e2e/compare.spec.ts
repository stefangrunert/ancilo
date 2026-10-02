import { expect, project, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-05
test("a blind comparison is started, reported and rated in the app; A/B tests are visible", async ({ page, daemon }) => {
  for (const a of ["demo/Coder-GGUF", "demo/Writer-GGUF"]) await daemon.op("add_model", { address: a, start: false }, true);
  await waitStatus(daemon, "writer-q8_0", "ready");
  const cwd = project();
  await daemon.openSystem(page);
  await page.getByTestId("section-compare").locator("summary").click();
  const form = page.getByTestId("section-compare");
  await form.getByLabel("Task").fill("Create NOTES.md");
  await form.getByLabel("Project folder").fill(cwd);
  await form.getByRole("checkbox", { name: "Coder" }).check();
  await form.getByRole("checkbox", { name: "Writer" }).check();
  await form.getByLabel("Check command (optional)").fill("grep -q '# Notes' NOTES.md");
  await form.getByRole("checkbox", { name: /Blind/ }).check();
  await form.getByRole("button", { name: "Compare" }).click();
  const report = page.locator("[data-testid^=report-]");
  await expect(report).toContainText("done", { timeout: 60_000 });
  // Blind: models hidden until rated.
  await expect(report).not.toContainText("coder-q8_0");
  await report.getByRole("button", { name: "A", exact: true }).click();
  await expect(report).toContainText(/coder-q8_0|writer-q8_0/);
  // A/B test started elsewhere shows up.
  await daemon.op("ab_start", { role: "default", b: "coder-q8_0", share: 20 });
  await page.reload();
  await page.getByTestId("section-compare").locator("summary").click();
  await expect(page.getByTestId("ab-tests")).toContainText("coder-q8_0");
});
