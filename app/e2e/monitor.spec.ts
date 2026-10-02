import { writeFileSync } from "node:fs";
import { expect, test, waitStatus } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

const GIB = 2 ** 30;

// covers: M7-AC-15
test("the status bar shows how the computer is doing and warns with fixes when it gets tight", async ({ page, daemon }) => {
  const total = (await daemon.op("resource_status")).total_bytes as number;
  await daemon.open(page);
  const bar = page.getByTestId("statusbar");
  await expect(bar.getByTestId("verdict")).toHaveText("All calm");
  await expect(bar).toContainText("Memory 50 %");
  await expect(page.getByTestId("monitor-warning")).toHaveCount(0);
  // A model runs; "Performance" leaves it loaded when memory gets tight – the user decides.
  await daemon.op("set_resources", { level: "performance" });
  await daemon.op("start_model", { model: "chat-q8_0" });
  await waitStatus(daemon, "chat-q8_0", "running");

  // Other programs fill the memory.
  const programs = [
    { name: "Google Chrome", memory_bytes: 9 * GIB, cpu_percent: 14, ancilo: false },
    { name: "Simulator", memory_bytes: 4 * GIB, cpu_percent: 3, ancilo: false },
  ];
  writeFileSync(daemon.probe, JSON.stringify({ available_bytes: Math.round(total * 0.02), pressure: "warn", thermal: "nominal", swap_used_bytes: 0, programs }));
  const card = page.getByTestId("monitor-warning");
  await expect(card).toBeVisible({ timeout: 10_000 });
  await expect(card).toContainText("Your computer is getting tight");
  await expect(card).toContainText("Mostly other programs: Google Chrome (9.0 GB), Simulator (4.0 GB).");
  await expect(bar.getByTestId("verdict")).toHaveText("Computer is busy");
  await expect(bar).toContainText("Memory 98 %");
  await expect(card.getByRole("button", { name: /^Unload the AI \(frees / })).toBeVisible();
  // One click: Ancilo takes less – on "Eco" it also makes room by itself, and says so.
  await card.getByRole("button", { name: "Switch to Eco" }).click();
  await expect.poll(async () => (await daemon.op("resource_status")).settings.level).toBe("eco");
  await expect(bar.getByRole("button", { name: "Eco" })).toBeVisible();
  const acted = page.getByTestId("monitor-acted");
  await expect(acted).toContainText("Memory was running out, so Ancilo unloaded", { timeout: 15_000 });
  await acted.getByRole("button", { name: "OK" }).click();
  await expect(acted).toHaveCount(0);
  // In tests the Activity Monitor is not really opened.
  await card.getByRole("button", { name: "Open Activity Monitor" }).click();
  await card.getByRole("button", { name: "Later" }).click();
  await expect(card).toHaveCount(0);

  // Worse: it is back, at once.
  writeFileSync(daemon.probe, JSON.stringify({ available_bytes: Math.round(total * 0.4), pressure: "normal", thermal: "critical", swap_used_bytes: 0, programs }));
  await expect(card).toContainText("Your computer is at its limit", { timeout: 10_000 });
  await expect(card).toContainText("The computer is very warm.");
  await expect(bar.getByTestId("verdict")).toHaveText("Computer at its limit");

  // Calm again: the warning goes by itself.
  writeFileSync(daemon.probe, JSON.stringify({ available_bytes: Math.round(total / 2), pressure: "normal", thermal: "nominal", swap_used_bytes: 0 }));
  await expect(card).toHaveCount(0, { timeout: 10_000 });
  await expect(bar.getByTestId("verdict")).toHaveText("All calm");
});
