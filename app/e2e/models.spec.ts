import { expect, project, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-04
test("one model keeps it simple; a second brings roles and recommendations", async ({ page, daemon }) => {
  await daemon.openSystem(page);
  await expect(page.getByTestId("model-chat-q8_0")).toBeVisible();
  await expect(page.getByTestId("section-models")).toHaveCount(0);
  await expect(page.getByText(/Roles|Rollen/)).toHaveCount(0);
  // A second chat model (added elsewhere) → the area appears by itself.
  await daemon.op("add_model", { address: "demo/Coder-GGUF", start: false }, true);
  await daemon.op("add_model", { address: "demo/Writer-GGUF", start: false }, true);
  await expect(page.getByTestId("section-models")).toBeVisible({ timeout: 15_000 });
  // Downloads finished: the list stops changing under the pointer.
  await waitStatus(daemon, "coder-q8_0", "ready");
  await waitStatus(daemon, "writer-q8_0", "ready");
  await page.getByTestId("section-models").locator("summary").click();
  await page.getByRole("combobox", { name: "Use for Coder" }).selectOption("coding");
  await expect(page.getByTestId("roles-coder-q8_0")).toContainText("coding");
  // Enough comparison data against the model in charge (the default) → a
  // recommendation with an Apply button.
  const cwd = project();
  const c = await daemon.op("compare_models", { task: "Write tests: create NOTES.md", cwd, models: ["chat-q8_0", "coder-q8_0"], check: "grep -q '# Notes' NOTES.md", repeat: 10, kind: "tests" }, true);
  await expect.poll(async () => (await daemon.op("comparison_status", { id: c.id })).status, { timeout: 60_000 }).toBe("done");
  await page.reload();
  await page.getByTestId("section-models").locator("summary").click();
  const apply = page.getByRole("button", { name: "Apply" });
  await expect(apply).toBeVisible({ timeout: 15_000 });
  await apply.click();
  await expect.poll(async () => (await daemon.op("list_routes")).map((r: { kind: string; model: string }) => `${r.kind}:${r.model}`)).toEqual(["tests:coder-q8_0"]);
});
