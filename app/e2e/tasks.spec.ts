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
  // No choice up front: a folder only when the task is about one.
  await expect(page.getByTestId("tasks-area").getByRole("combobox")).toHaveCount(0);
  await dialogPicks(page, dir);
  await page.getByRole("button", { name: "Choose a folder" }).click();
  await expect(page.getByTestId("task-folder-chip")).toContainText(`Works in: ${dir.split("/").pop()}`);
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  const changes = page.getByTestId("task-changes");
  await expect(changes).toBeVisible({ timeout: 20_000 });
  await expect(changes.getByTestId("task-change")).toHaveCount(2);
  await expect(changes).toContainText("from power.txt");
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(false);
  // The folder is listed in the Tasks area from now on.
  await expect(page.getByRole("list", { name: "Folders" })).toContainText(dir.split("/").pop()!);
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
test("a task with files makes something new from them – saved where the user wants", async ({ page, daemon }) => {
  const out = tmp("ancilo-results-");
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  await page.locator('input[type="file"]').setInputFiles({ name: "power.txt", mimeType: "text/plain", buffer: Buffer.from("Power: 120 Euro") });
  await expect(page.getByTestId("task-files")).toContainText("power.txt");
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  const results = page.getByTestId("task-results");
  await expect(results).toBeVisible({ timeout: 20_000 });
  await expect(results).toContainText("Overview.xlsx");
  // The file given stays material – no folder shown, no "keep".
  await expect(results.getByRole("button", { name: "Keep" })).toHaveCount(0);
  await dialogPicks(page, out);
  await results.getByRole("button", { name: "Somewhere else…" }).click();
  const saved = page.getByTestId("task-saved");
  await expect(saved).toContainText(`in “${out.split("/").pop()}”`);
  expect(readdirSync(out)).toContain("Overview.xlsx");
  await expect(page.getByRole("list", { name: "Tasks", exact: true })).toContainText("Make a table of the invoices");
});
