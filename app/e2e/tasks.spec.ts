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
test("a failed result check can be retried before keeping the files", async ({ page, daemon }) => {
  const dir = tmp("ancilo-task-retry-");
  writeFileSync(join(dir, "power.txt"), "Power: 120 Euro");
  let fail = true;
  await page.route("**/api/v1/ops/check_results", (r) => fail
    ? r.fulfill({ status: 409, json: { error: { code: "conflict", message: "Could not read the results" } } })
    : r.continue());
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  await dialogPicks(page, dir);
  await page.getByRole("button", { name: "Choose a folder" }).click();
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Make a table of the invoices");
  await ask.press("Enter");
  const changes = page.getByTestId("task-changes");
  await expect(changes).toContainText("Could not read the results", { timeout: 20_000 });
  await expect(changes.getByRole("button", { name: "Keep" })).toBeDisabled();
  await expect(changes.getByTestId("check-badge")).toHaveCount(0);
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(false);
  fail = false;
  await changes.getByRole("button", { name: "Check again" }).click();
  await expect(changes.getByRole("button", { name: "Keep" })).toBeEnabled();
  await changes.getByRole("button", { name: "Keep" }).click();
  await expect(page.getByTestId("task-applied")).toBeVisible();
  expect(existsSync(join(dir, "Overview.xlsx"))).toBe(true);
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

// covers: M8-AC-06, M10-AC-04
test("lost start, upload and send replies recover into one task and one turn", async ({ page, daemon }) => {
  const dir = tmp("ancilo-task-lost-");
  const loseOnce = async (pattern: string) => {
    let lost = false;
    await page.route(pattern, async (route) => {
      if (lost) return route.continue();
      lost = true;
      await route.fetch(); // The daemon accepted it; only the reply is lost.
      await route.abort("failed");
    });
  };
  await loseOnce("**/api/v1/ops/create_task");
  await loseOnce("**/api/v1/sessions/*/files?*");
  await loseOnce("**/api/v1/ops/send_message");
  await daemon.open(page);
  await page.getByRole("tab", { name: "Tasks" }).click();
  await dialogPicks(page, dir);
  await page.getByRole("button", { name: "Choose a folder" }).click();
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  const file = { name: "power.txt", mimeType: "text/plain", buffer: Buffer.from("Power: 120 Euro") };
  await page.locator('input[type="file"]').setInputFiles(file);
  await ask.fill("Make a table of the invoices");
  // Creation failed to acknowledge. Reload must retain the draft and ID.
  await ask.press("Enter");
  await expect(page.getByRole("alert")).toBeVisible();
  await page.reload();
  await expect(ask).toHaveValue("Make a table of the invoices");
  // Selecting the same file after reload may have a new browser timestamp.
  await page.locator('input[type="file"]').setInputFiles(file);
  for (let i = 0; i < 2; i++) {
    await ask.press("Enter");
    await expect(page.getByRole("alert")).toBeVisible();
    await expect(ask).toHaveValue("Make a table of the invoices");
  }
  await ask.press("Enter");
  await expect(page.getByTestId("task-changes")).toBeVisible({ timeout: 20_000 });
  const sessions = await daemon.op("list_sessions");
  expect(sessions).toHaveLength(1);
  expect(sessions[0].turns).toBe(1);
  expect(readdirSync(dir)).toEqual([]);
});

test.describe("interrupted tasks", () => {
  test.use({ daemonArgs: ["--with-models", "--tasks", "--task-interrupted"] });

  // covers: M8-AC-06, M10-AC-04
  test("stop, reconnect and continue retain the request and working copy", async ({ page, daemon, context }) => {
    const dir = tmp("ancilo-task-interrupted-");
    await daemon.open(page);
    await page.getByRole("tab", { name: "Tasks" }).click();
    await dialogPicks(page, dir);
    await page.getByRole("button", { name: "Choose a folder" }).click();
    const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
    await ask.fill("Write my note");
    await ask.press("Enter");
    const task = page.getByTestId("task");
    await expect(task).toHaveAttribute("data-status", "running");
    await expect(task.getByTestId("user-message")).toContainText("Write my note");
    await expect.poll(async () => {
      const [s] = await daemon.op("list_sessions");
      return (await daemon.op("get_session", { session: s.id })).changes.length;
    }).toBe(1);
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    await expect(task).toHaveAttribute("data-status", "interrupted");
    await context.setOffline(true);
    // Browser offline mode blocks new requests, but may leave an existing
    // localhost event stream open. Exercise the failed request itself.
    await page.getByRole("button", { name: "Continue task" }).click();
    await expect(page.getByRole("alert")).toBeVisible();
    await expect(task).toHaveAttribute("data-status", "interrupted");
    await context.setOffline(false);
    await page.reload();
    await expect(task).toHaveAttribute("data-status", "interrupted");
    await expect(task.getByTestId("user-message")).toContainText("Write my note");
    await page.getByRole("button", { name: "Continue task" }).click();
    await expect(task).toHaveAttribute("data-status", "idle");
    await expect(task).toContainText("Continued from the existing files.");
    const [s] = await daemon.op("list_sessions");
    expect(s.turns).toBe(2);
    expect(s.changes.map((f: { path: string }) => f.path)).toEqual(["partial.txt"]);
    expect(readdirSync(dir)).toEqual([]);
  });
});
