import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models", "--everyday-chat"] });

// covers: M6-AC-11, M7-AC-02, M7-AC-11
test("everyday chat keeps follow-ups and drafts, filters titles, and copies answers", async ({ page, daemon }, testInfo) => {
  // Clipboard permissions differ between engines. Inspect precisely what the
  // real button writes; clipboard denial is exercised in the component test.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText: async (text: string) => { (window as any).copiedAnswer = text; } } });
  });
  await daemon.open(page);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await box.fill("Plan a picnic");
  await page.reload();
  await expect(box).toHaveValue("Plan a picnic");

  // Hold the first response so there is time to prepare the next question.
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/v1/ops/ask", async (route) => {
    const response = await route.fetch();
    await gate;
    await route.fulfill({ response });
  }, { times: 1 });
  await box.press("Enter");
  await expect(page.getByTestId("user-message")).toHaveText("Plan a picnic");
  await box.fill("Make it vegan");
  release();
  await expect(page).toHaveURL(/#\/chat\/c-/);
  const url = page.url();
  await expect(box).toHaveValue("Make it vegan");
  await box.press("Enter");
  await expect(page.getByTestId("assistant-answer").last()).toContainText("Replace the cheese with hummus");
  await page.getByRole("button", { name: "Copy answer", exact: true }).last().click();
  expect(await page.evaluate(() => (window as any).copiedAnswer)).toBe("Replace the cheese with hummus. Keep the bread and fruit.");
  await expect(page.getByRole("status").filter({ hasText: "Copied" })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("chat.png"), fullPage: true });

  await box.fill("What if it rains?");
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(box).toHaveValue("");
  await box.fill("A completely different question");
  const chats = page.getByRole("list", { name: "Chats", exact: true });
  const filter = page.getByRole("searchbox", { name: "Filter chat titles" });
  await filter.fill("  PICNIC  ");
  await expect(chats.getByRole("listitem")).toHaveCount(1);
  await filter.fill("no such topic");
  await expect(chats.getByRole("listitem")).toHaveCount(0);
  await expect(page.getByText("No matching chat titles.")).toBeVisible();
  await filter.press("Escape");
  await chats.getByRole("button", { name: "Plan a picnic", exact: true }).click();
  await expect(page).toHaveURL(url);
  await expect(box).toHaveValue("What if it rains?");
  await page.reload();
  await expect(box).toHaveValue("What if it rains?");
  await expect(page.getByTestId("assistant-answer")).toHaveCount(2);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(box).toHaveValue("A completely different question");
  expect((await daemon.op("list_conversations")).length).toBe(1);
});

// covers: M7-AC-02, M7-AC-11
test("a failed first send restores both the request and text typed while waiting", async ({ page, daemon }) => {
  await daemon.open(page);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/v1/ops/ask", async (route) => {
    await gate;
    await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: { code: "unavailable", message: "Test connection lost" } }) });
  }, { times: 1 });
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await box.fill("Plan a picnic");
  await box.press("Enter");
  await expect(page.getByTestId("user-message")).toBeVisible();
  await box.fill("For six people");
  release();
  await expect(page.getByText("Test connection lost")).toBeVisible();
  await expect(box).toHaveValue("Plan a picnic\n\nFor six people");
  await page.reload();
  await expect(box).toHaveValue("Plan a picnic\n\nFor six people");
});

// covers: M7-AC-11
test("a completed answer does not pull the user back from another view", async ({ page, daemon }) => {
  await daemon.open(page);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/v1/ops/ask", async (route) => {
    const response = await route.fetch();
    await gate;
    await route.fulfill({ response });
  }, { times: 1 });
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await box.fill("Plan a picnic");
  await box.press("Enter");
  await expect(page.getByTestId("user-message")).toBeVisible();
  await page.getByRole("button", { name: "System", exact: true }).click();
  const response = page.waitForResponse("**/api/v1/ops/ask");
  release();
  await response;
  await expect(page.getByRole("list", { name: "Chats", exact: true })).toContainText("Plan a picnic");
  await expect(page).toHaveURL(/#\/system$/);
  await page.getByRole("list", { name: "Chats", exact: true }).getByRole("button", { name: "Plan a picnic", exact: true }).click();
  await expect(page.getByTestId("assistant-answer")).toContainText("cheese and fruit");
});
