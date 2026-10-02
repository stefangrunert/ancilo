import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models"] });

// covers: M7-AC-07
test("no serious accessibility findings and full keyboard use", async ({ page, daemon, browserName }) => {
  await daemon.op("add_model", { address: "demo/Coder-GGUF", start: false }, true);
  await daemon.openSystem(page);
  await expect(page.getByTestId("section-models")).toBeVisible();
  const scan = await new AxeBuilder({ page }).analyze();
  const serious = scan.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
  expect(serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  // Keyboard: from the top, Tab reaches the navigation, the models and the
  // connect buttons – and a chat is opened and asked with the keyboard alone.
  // Safari (WebKit) moves to buttons with Option+Tab, like every macOS app.
  const tab = browserName === "webkit" ? "Alt+Tab" : "Tab";
  await page.keyboard.press(tab);
  const reached: string[] = [];
  for (let i = 0; i < 25; i++) {
    reached.push(await page.evaluate(() => (document.activeElement?.getAttribute("aria-label") ?? document.activeElement?.textContent ?? "").trim()));
    await page.keyboard.press(tab);
  }
  expect(reached).toContain("New chat");
  expect(reached.some((r) => r.includes("Claude Code"))).toBe(true);
  await page.getByRole("button", { name: "New chat" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("textbox", { name: "What should Ancilo do?" })).toBeFocused();
  await page.keyboard.type("Use coder");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("assistant-answer")).toBeVisible({ timeout: 20_000 });
});

// covers: M7-AC-09
test("the app is ready to use quickly", async ({ page, daemon }) => {
  const t0 = Date.now();
  await daemon.open(page);
  const ms = Date.now() - t0;
  const nav = await page.evaluate(() => {
    const n = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming;
    return { dcl: n.domContentLoadedEventEnd, size: n.transferSize };
  });
  console.log(`ready in ${ms} ms (DOMContentLoaded ${Math.round(nav.dcl)} ms)`);
  expect(ms).toBeLessThan(3000);
});
