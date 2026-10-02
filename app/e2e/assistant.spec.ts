import { expect, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-03
test("the assistant proposes, the user confirms, the effect is visible", async ({ page, daemon }) => {
  await daemon.op("add_model", { address: "demo/Coder-GGUF", start: false }, true);
  await waitStatus(daemon, "coder-q8_0", "ready");
  await daemon.op("set_preferences", { view: "pro" });
  await daemon.open(page);
  await page.getByRole("button", { name: "New chat" }).click();
  const ask = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await ask.fill("Use the coder model for delegated tasks");
  await ask.press("Enter");
  await expect(page.getByTestId("assistant-answer")).toContainText("please confirm", { timeout: 20_000 });
  const proposal = page.getByRole("group", { name: "Ancilo proposes" });
  await expect(proposal).toContainText("assign_role");
  // Nothing happened yet.
  expect((await daemon.op("explain_route", { role: "delegation" })).model).not.toBe("coder-q8_0");
  await expect(proposal).toContainText("From now on coder-q8_0 takes care of the tasks Claude Code or Codex hand over.");
  await proposal.getByRole("button", { name: "Yes, do it" }).click();
  await expect(proposal).toContainText("Done.");
  await page.getByRole("button", { name: "System", exact: true }).click();
  await page.getByTestId("section-models").locator("summary").click();
  await expect(page.getByTestId("roles-coder-q8_0")).toContainText("delegation");
});
