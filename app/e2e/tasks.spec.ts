import { existsSync, mkdtempSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models", "--tasks"] });

const tmp = (prefix: string) => realpathSync(mkdtempSync(join(tmpdir(), prefix)));

/** The system's folder dialog answers with `dir` (no real dialog in tests). */
async function dialogPicks(page: Page, dir: string) {
  await page.route("**/api/v1/ops/choose_folder", (r) => r.fulfill({ json: { path: dir } }));
}

// covers: M10-AC-04, M10-AC-05
test("a task in a folder works in a copy: the changes are shown, kept – and undone", async ({ page, daemon }) => {
  const dir = tmp("ancilo-task-");
  writeFileSync(join(dir, "power.txt"), "Power: 120 Euro");
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  // The folder comes first: until then there is nothing to type.
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await expect(ask).toBeDisabled();
  await dialogPicks(page, dir);
  await page.getByRole("button", { name: "Choose a folder" }).click();
  await expect(page.getByTestId("task-folder-chosen")).toContainText(dir.split("/").pop()!);
  await expect(page.getByTestId("task-folder-chosen")).toContainText("only what you keep goes into the folder");
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  const changes = page.getByTestId("task-changes");
  await expect(changes).toBeVisible({ timeout: 20_000 });
  await expect(changes.getByTestId("task-change")).toHaveCount(2);
  await expect(changes).toContainText("from power.txt");
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(false);
  // The folder is listed in the Tasks area from now on.
  await expect(page.getByRole("list", { name: "Folders" })).toContainText(dir.split("/").pop()!);
  // covers: FPL-03 – checked and looked at before keeping.
  const sheet = changes.getByTestId("task-change").filter({ hasText: "Overview.xlsx" });
  await expect(sheet.getByTestId("check-badge")).toContainText("checked", { timeout: 20_000 });
  await sheet.getByRole("button", { name: "Look at it" }).click();
  const preview = page.getByTestId("result-preview");
  await expect(preview.locator("table")).toContainText("Power");
  await expect(preview.getByTestId("check-findings")).toContainText("the file opens and can be read");
  await expect(preview.getByTestId("preview-limits")).toContainText("Fonts, colours");
  await preview.getByRole("button", { name: "Close" }).click();
  await changes.getByRole("button", { name: "Keep" }).click();
  const applied = page.getByTestId("task-applied");
  await expect(applied).toContainText("Kept: 2 change(s)");
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(true);
  await applied.getByRole("button", { name: "Undo" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Undo" }).click();
  await expect(applied).toHaveCount(0);
  await expect.poll(() => readdirSync(dir).sort()).toEqual(["power.txt"]);
});

// covers: M10-AC-04
test("an example starts a task: the folder first, then the files given go into its copy", async ({ page, daemon }) => {
  const dir = tmp("ancilo-task-files-");
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  // An example without a folder asks for the folder first – then fills the input.
  await dialogPicks(page, dir);
  await page.getByRole("group", { name: "For example:" }).getByRole("button", { name: "Make a table of the invoices" }).click();
  await expect(page.getByTestId("task-folder-chosen")).toContainText(dir.split("/").pop()!);
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await expect(ask).toHaveValue("Make a table of the invoices");
  await page.locator('input[type="file"]').setInputFiles({ name: "power.txt", mimeType: "text/plain", buffer: Buffer.from("Power: 120 Euro") });
  await expect(page.getByTestId("task-files")).toContainText("power.txt");
  await ask.press("Enter");
  const changes = page.getByTestId("task-changes");
  await expect(changes).toBeVisible({ timeout: 20_000 });
  await expect(changes).toContainText("Overview.xlsx");
  // Nothing in the folder until the user keeps it.
  expect(readdirSync(dir)).toEqual([]);
  await changes.getByRole("button", { name: "Keep" }).click();
  await expect(page.getByTestId("task-applied")).toBeVisible();
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(true);
  expect(existsSync(join(dir, "2025", "power.txt"))).toBe(true);
});
