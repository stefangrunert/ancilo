import { expect, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-13
test("the cockpit sets how much Ancilo may take and shows what it takes", async ({ page, daemon }) => {
  await daemon.openSystem(page, false);
  const cockpit = page.getByTestId("cockpit");
  await expect(cockpit.getByRole("slider")).toHaveAttribute("aria-valuetext", "Balanced");
  await expect(cockpit).toContainText("No model is loaded");
  await expect(page.getByRole("button", { name: /Ancilo is idle/ })).toBeVisible();
  // A model runs: the cockpit and the sidebar show it.
  await daemon.op("start_model", { model: "chat-q8_0" });
  await waitStatus(daemon, "chat-q8_0", "running");
  await expect(cockpit).toContainText(/unloaded in 1\d min without use/, { timeout: 10_000 });
  await expect(page.getByRole("button", { name: /Ancilo uses/ })).toBeVisible();
  // Eco: one click, applied in the daemon.
  await cockpit.getByRole("button", { name: "Eco" }).click();
  await expect(cockpit).toContainText("Ancilo holds back");
  await expect.poll(async () => (await daemon.op("resource_status")).settings.level).toBe("eco");
  // The handle can be dragged with the mouse: to the far right is the maximum.
  const slider = cockpit.getByRole("slider");
  const box = (await slider.boundingBox())!;
  await page.mouse.move(box.x + 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.5, box.y + box.height / 2, { steps: 5 });
  await page.mouse.move(box.x + box.width - 1, box.y + box.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect(slider).toHaveAttribute("aria-valuetext", "Maximum");
  await expect.poll(async () => (await daemon.op("resource_status")).settings.level).toBe("max");
  // Unload everything.
  await cockpit.getByRole("button", { name: "Unload all now" }).click();
  await waitStatus(daemon, "chat-q8_0", "ready");
  await expect(cockpit).toContainText("No model is loaded", { timeout: 10_000 });
});
