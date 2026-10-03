import { mkdtempSync, readdirSync, realpathSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models", "--tasks"] });

// covers: M10-AC-04, M10-AC-05
test("a task works in a copy: the changes are shown, kept into the folder – and undone", async ({ page, daemon }) => {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "ancilo-task-")));
  writeFileSync(join(dir, "power.txt"), "Power: 120 Euro");
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  // A folder for tasks.
  await page.getByRole("navigation", { name: "Navigation" }).getByRole("button", { name: "Add a folder" }).click();
  await page.getByText("For experts: type a folder path").click();
  await page.getByRole("textbox", { name: "Folder" }).fill(dir);
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.getByTestId("task-folder")).toBeVisible();
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  // The task's view: what changed – only in the copy so far.
  const changes = page.getByTestId("task-changes");
  await expect(changes).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("messages")).toContainText("Done: Overview.xlsx");
  await expect(changes.getByTestId("task-change")).toHaveCount(2);
  await expect(changes).toContainText("Overview.xlsx");
  await expect(changes).toContainText("from power.txt");
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(false);
  // Listed under its folder in the Tasks area.
  await expect(page.getByRole("list", { name: "Folders" })).toContainText(dir.split("/").pop()!);
  // Keep: into the folder.
  await changes.getByRole("button", { name: "Keep" }).click();
  const applied = page.getByTestId("task-applied");
  await expect(applied).toContainText("Kept: 2 change(s)");
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(true);
  expect(existsSync(join(dir, "2025", "power.txt"))).toBe(true);
  // Undo: as it was.
  await applied.getByRole("button", { name: "Undo" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Undo" }).click();
  await expect(applied).toHaveCount(0);
  await expect.poll(() => readdirSync(dir).sort()).toEqual(["power.txt"]);
});

// covers: M10-AC-04
test("a new task without a folder gets one of its own and takes the files given to it", async ({ page, daemon }) => {
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  await page.locator('input[type="file"]').setInputFiles({ name: "power.txt", mimeType: "text/plain", buffer: Buffer.from("Power: 120 Euro") });
  await expect(page.getByTestId("task-files")).toContainText("power.txt");
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  const changes = page.getByTestId("task-changes");
  await expect(changes).toBeVisible({ timeout: 20_000 });
  // The file given and what the agent made of it – all new in the task's own folder.
  await expect(changes).toContainText("Overview.xlsx");
  await expect(changes).toContainText("2025/power.txt");
  await expect(page.getByRole("list", { name: "Tasks", exact: true })).toContainText("Make a table of the invoices");
});
