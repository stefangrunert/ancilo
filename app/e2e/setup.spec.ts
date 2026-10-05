import { expect, test, waitStatus } from "./harness";

// covers: M7-AC-01, M7-AC-12, M7-AC-14
test("a layperson is led through the setup – the AI runs and Claude Code is connected in few inputs", async ({ page, daemon }) => {
  await daemon.open(page);
  let inputs = 0;
  const setup = page.getByTestId("setup");
  await expect(setup.getByRole("heading", { name: "Set up" })).toBeVisible();
  // 1: what for – "chat and write" is preselected; each step explains itself.
  const purpose = page.getByTestId("step-purpose");
  await expect(purpose).toContainText("Step 1 of 5");
  await expect(purpose).toContainText("Ancilo picks the AI that suits your purpose");
  await expect(page.getByRole("checkbox", { name: /Chat and write/ })).toBeChecked();
  await page.getByRole("button", { name: "Next" }).click();
  inputs++;
  // 2: the AI that suits this computer – in plain words, one click.
  const modelStep = page.getByTestId("step-model");
  await expect(modelStep).toContainText("The AI runs entirely on your computer");
  const best = page.getByTestId("suggestion-chat");
  await expect(best).toContainText("Recommended for you");
  await expect(best).toContainText("A good all-rounder.");
  await expect(best).toContainText("Tested by Ancilo");
  await expect(best).toContainText(/2 MB · about 1 min/);
  await expect(modelStep).toContainText(/Your computer: .* · 64 GB memory/);
  // No jargon on the way: no quantization, no address, no file format.
  await expect(setup).not.toContainText(/Q8_0|Q4_K|hf\.co|GGUF|quantiz/i);
  await best.getByRole("button", { name: "Download and start" }).click();
  inputs++;
  await expect(page.getByTestId("model-ready")).toContainText("The AI is ready.", { timeout: 20_000 });
  await waitStatus(daemon, "chat-q8_0", "running");
  // Claude Code from the system page: two more inputs.
  await page.getByRole("button", { name: "System", exact: true }).click();
  inputs++;
  await page.getByRole("button", { name: "Connect Claude Code" }).click();
  inputs++;
  await expect(page.getByTestId("connection-claude_code")).toContainText("connected");
  await expect(page.getByRole("button", { name: "Disconnect Claude Code" })).toBeVisible();
  expect(inputs).toBeLessThanOrEqual(4);
  const conn = await daemon.op("connections");
  expect(conn.find((c: { client: string }) => c.client === "claude_code").verified).toBe(true);
  // Back to the setup: it continues where it was.
  await page.getByRole("button", { name: "Set up", exact: true }).click();
  await page.getByRole("button", { name: "Next" }).click();
  const res = page.getByTestId("step-resources");
  await expect(res).toContainText("Step 3 of 5");
  await res.getByText("Eco", { exact: true }).click();
  await expect.poll(async () => (await daemon.op("resource_status")).settings.level).toBe("eco");
  await page.getByRole("button", { name: "Next" }).click();
  // Claude Code is connected already: on.
  await page.getByTestId("step-connect").getByRole("button", { name: "Next" }).click();
  await page.getByTestId("step-projects").getByRole("button", { name: "Skip" }).click();
  const list = page.getByTestId("setup-checklist");
  await expect(list).toContainText("Ancilo is set up.");
  await expect(list).toContainText("Claude Code");
  await expect(list).toContainText("skipped");
  // The setup stays the start page – now as the checklist.
  await page.reload();
  await expect(page.getByTestId("setup-checklist")).toBeVisible();
  // No "Ask something now" in the checklist: the Chat tab is right there.
  await expect(list.getByRole("button", { name: "Ask something now" })).toHaveCount(0);
});

// covers: M7-AC-12
test("what Ancilo is for decides the recommendation", async ({ page, daemon }) => {
  await daemon.open(page);
  await page.getByRole("checkbox", { name: /Programming/ }).check();
  await page.getByRole("checkbox", { name: /Chat and write/ }).uncheck();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByTestId("suggestion-coder")).toContainText("Recommended for you");
  await expect(page.getByTestId("suggestion-chat")).toHaveCount(0);
  // Back, and documents bring the search model along.
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("checkbox", { name: /Work with my documents/ }).check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByTestId("step-model")).toContainText("Ancilo also loads a small search model");
});
