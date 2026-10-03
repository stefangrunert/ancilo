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
