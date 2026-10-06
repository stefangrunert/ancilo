import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M10-AC-02
test("a document is attached to a chat, read on this computer and goes with the question", async ({ page, daemon }) => {
  await daemon.open(page);
  await page.getByRole("button", { name: "New chat" }).click();
  // Chosen with the paperclip (here: handed to its file field).
  await page.locator('input[type="file"]').setInputFiles({
    name: "Mietvertrag.md",
    mimeType: "text/markdown",
    buffer: Buffer.from("# Mietvertrag\n\nDie Miete beträgt 950 Euro im Monat."),
  });
  const chip = page.getByTestId("doc-chip");
  await expect(chip).toContainText("Mietvertrag.md");
  await expect(chip).toContainText("Text");
  await expect(page.getByTestId("local-note")).toContainText("Stays on this computer");
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await box.fill("Was kostet die Miete?");
  await box.press("Enter");
  await expect(page.getByTestId("assistant-answer")).toBeVisible({ timeout: 20_000 });
  // The question carries its document; the conversation stays local.
  await expect(page.getByTestId("user-message").getByTestId("doc-chip")).toContainText("Mietvertrag.md");
  await expect(page.getByTestId("local-note")).toBeVisible();
  const [c] = await daemon.op("list_conversations");
  const conv = await daemon.op("get_conversation", { id: c.id });
  expect(conv.messages[0].attachments[0].name).toBe("Mietvertrag.md");
  expect(conv.messages[1].documents).toBe(true);
});

// covers: M10-AC-03
test("a folder becomes a chat project: read, with what could not be read, and its chats answer from it", async ({ page, daemon }) => {
  const { mkdtempSync, writeFileSync, realpathSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "ancilo-docs-")));
  writeFileSync(join(dir, "Mietvertrag.md"), "# Mietvertrag\n\nDie Kündigungsfrist beträgt drei Monate.");
  writeFileSync(join(dir, "kaputt.pdf"), "kein pdf");
  await daemon.open(page);
  await page.getByRole("button", { name: "Add a folder" }).click();
  await page.getByText("For experts: type a folder path").click();
  await page.getByRole("textbox", { name: "Folder" }).fill(dir);
  await page.getByRole("button", { name: "Add", exact: true }).click();
  const status = page.getByTestId("folder-status");
  await expect(status).toContainText("1 documents read", { timeout: 20_000 });
  await expect(status).toContainText("1 file(s) not read");
  await status.getByText("1 file(s) not read").click();
  await expect(page.getByTestId("not-read")).toContainText("kaputt.pdf");
  // Listed in the chat area's projects.
  const projects = page.getByRole("list", { name: "Projects" });
  await expect(projects).toContainText(dir.split("/").pop()!);
  const ask = page.getByRole("textbox", { name: "Ask about these documents" });
  await ask.fill("Wie lang ist die Kündigungsfrist?");
  await ask.press("Enter");
  await expect(page.getByTestId("assistant-answer")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("local-note")).toBeVisible();
  // The chat belongs to the project – not to the plain chats.
  await expect(projects.getByRole("button", { name: "Wie lang ist die Kündigungsfrist?", exact: true })).toBeVisible();
  await expect(page.getByRole("list", { name: "Chats", exact: true })).not.toContainText("Kündigungsfrist");
  const [c] = await daemon.op("list_conversations");
  expect(c.folder).toBe(dir);
});

test.describe("sources", () => {
  test.use({ daemonArgs: ["--with-models", "--docs"] });

  // covers: FPL-01
  test("an answer from a folder names its source: it opens the passage; a made-up one is taken out", async ({ page, daemon }) => {
    const { mkdtempSync, writeFileSync, realpathSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const dir = realpathSync(mkdtempSync(join(tmpdir(), "ancilo-sources-")));
    writeFileSync(join(dir, "Mietvertrag.md"), "# Mietvertrag\n\n§ 9 Kündigung: Die Kündigungsfrist beträgt drei Monate zum Monatsende.");
    await daemon.open(page);
    await page.getByRole("button", { name: "Add a folder" }).click();
    await page.getByText("For experts: type a folder path").click();
    await page.getByRole("textbox", { name: "Folder" }).fill(dir);
    await page.getByRole("button", { name: "Add", exact: true }).click();
    await expect(page.getByTestId("folder-status")).toContainText("1 documents read", { timeout: 20_000 });
    const ask = page.getByRole("textbox", { name: "Ask about these documents" });
    await ask.fill("Wie lang ist die Kündigungsfrist?");
    await ask.press("Enter");
    const answer = page.getByTestId("assistant-answer");
    await expect(answer).toContainText("Die Kündigungsfrist beträgt drei Monate", { timeout: 20_000 });
    // The made-up source is gone from the text, and that is said.
    await expect(answer).not.toContainText("D9");
    await expect(page.getByTestId("dropped-marks")).toContainText("removed 1 source");
    await expect(page.getByTestId("doc-sources")).toContainText("Mietvertrag.md");
    // The mark opens the passage the answer had.
    await answer.getByRole("button", { name: "Open source 1" }).click();
    const view = page.getByTestId("source-view");
    await expect(view.getByTestId("source-excerpt")).toContainText("drei Monate zum Monatsende");
    await expect(view.getByTestId("source-changed")).toHaveCount(0);
    await view.getByRole("button", { name: "Close" }).click();
    // The file changes: the old source says so – and still shows what the answer had.
    writeFileSync(join(dir, "Mietvertrag.md"), "# Mietvertrag\n\n§ 9 Kündigung: sechs Monate.");
    const [c] = await daemon.op("list_conversations");
    await daemon.op("read_folder", { folder: dir });
    await expect
      .poll(async () => (await daemon.op("open_evidence", { conversation: c.id, mark: "D1" })).now, { timeout: 20_000 })
      .toBe("changed");
    await page.getByTestId("source-D1").click();
    await expect(view.getByTestId("source-changed")).toContainText("has changed since the answer");
    await expect(view.getByTestId("source-excerpt")).toContainText("drei Monate zum Monatsende");
  });
});
