import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-11
test("the app works like a chat app: conversations on the left, a real multi-line chat on the right", async ({ page, daemon }) => {
  await daemon.open(page);
  // The overview has no chat of its own: "New chat" opens one.
  await expect(page.getByRole("textbox", { name: "What should Ancilo do?" })).toHaveCount(0);
  await page.getByRole("button", { name: "New chat" }).click();
  await expect(page.getByRole("heading", { name: "Ancilo Chat" })).toBeVisible();
  const start = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await start.fill("Use the coder model");
  await start.press("Shift+Enter");
  await start.pressSequentially("for delegated tasks");
  await start.press("Enter");
  await expect(page).toHaveURL(/#\/chat\/c-/);
  const answer = page.getByTestId("assistant-answer");
  await expect(answer).toContainText("please confirm", { timeout: 20_000 });
  // The answer as Markdown, the request as the user's bubble with its line break.
  await expect(answer.locator("strong")).toHaveText("coder-q8_0");
  expect(await page.getByTestId("user-message").innerText()).toBe("Use the coder model\nfor delegated tasks");
  // The input sits below the conversation.
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  const [composerTop, answerBottom] = [(await box.boundingBox())!.y, (await answer.boundingBox())!];
  expect(composerTop).toBeGreaterThan(answerBottom.y + answerBottom.height);
  // A follow-up continues the same conversation.
  await box.fill("And once more");
  await box.press("Enter");
  await expect(page.getByTestId("assistant-answer")).toHaveCount(2, { timeout: 20_000 });
  const chats = page.getByRole("list", { name: "Chats" });
  await expect(chats.getByRole("listitem")).toHaveCount(1);
  await expect(chats).toContainText("Use the coder model");
  // Kept: after a reload, and when opened from the list.
  await page.reload();
  await expect(page.getByTestId("user-message")).toHaveCount(2);
  await page.getByRole("button", { name: "System", exact: true }).click();
  await expect(page.getByTestId("user-message")).toHaveCount(0);
  await chats.getByRole("button", { name: /Use the coder model/ }).click();
  await expect(page.getByTestId("assistant-answer")).toHaveCount(2);
  expect((await daemon.op("list_conversations")).length).toBe(1);
});

test.describe("in dark mode", () => {
  test.use({ colorScheme: "dark" });
  test("the background is a grey, not black", async ({ page, daemon }) => {
    await daemon.open(page);
    const rgb = await page.evaluate(() => getComputedStyle(document.body).backgroundColor.match(/\d+/g)!.slice(0, 3).map(Number));
    expect(Math.min(...rgb)).toBeGreaterThanOrEqual(36);
    expect(Math.max(...rgb)).toBeLessThan(80);
  });
});
