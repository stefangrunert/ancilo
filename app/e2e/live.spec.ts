import { expect, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-06
test("changes made elsewhere appear without reloading", async ({ page, daemon }) => {
  await daemon.openSystem(page);
  const status = page.getByTestId("model-chat-q8_0").getByTestId("model-status");
  await expect(status).toHaveText(/ready/);
  await daemon.op("start_model", { model: "chat-q8_0" });
  await expect(status).toHaveText(/running/, { timeout: 15_000 });
  await daemon.op("stop_model", { model: "chat-q8_0" });
  await expect(status).toHaveText(/ready/, { timeout: 15_000 });
  await waitStatus(daemon, "chat-q8_0", "ready");
  // Settings too.
  await daemon.op("set_permissions", { max_access: "read" });
  await expect(page.getByRole("radio", { name: "read" })).toBeChecked({ timeout: 15_000 });
});
