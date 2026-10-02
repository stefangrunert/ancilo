import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./harness";

test.use({ daemonArgs: ["--with-models", "--web"] });

// covers: M6-AC-14, M7-AC-16
test("web search is off until chosen, asks before searching and answers with sources", async ({ page, daemon }) => {
  await daemon.open(page);
  // The system page says it is off – and leads to its page.
  await page.getByRole("button", { name: "System", exact: true }).click();
  // A line like a model's: off, nothing goes out.
  const status = page.getByTestId("web-status");
  await expect(status).toContainText("Off");
  await expect(status).toContainText("nothing goes out");
  await status.getByRole("button", { name: "Set up web search" }).click();
  const web = page.getByTestId("web-search");
  await expect(web).toContainText("those websites see your internet address");
  const scan = await new AxeBuilder({ page }).analyze();
  expect(scan.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => v.id)).toEqual([]);
  await web.locator("label", { hasText: /^Wikipedia/ }).click();
  await expect.poll(async () => (await daemon.op("get_web_search")).provider).toBe("wikipedia");
  await expect(web.getByRole("radio", { name: /Ask me first/ })).toBeChecked();
  await page.getByRole("button", { name: "System", exact: true }).click();
  await expect(status).toContainText("Wikipedia");
  await expect(status).toContainText("asks before searching");
  await expect(status.locator(".dot-running")).toHaveCount(1);
  await status.getByRole("button", { name: "Change" }).click();
  await web.getByRole("button", { name: "Try a search" }).click();
  // The fake Wikipedia has no article on that – the test search says so in plain words.
  await expect(web.getByRole("alert")).toContainText("found nothing");

  // A question that needs facts: Ancilo shows the query first.
  await page.getByRole("button", { name: "New chat" }).click();
  const box = page.getByRole("textbox", { name: "What should Ancilo do?" });
  await box.fill("Wie viele Einwohner hat Oslo?");
  await box.press("Enter");
  const proposal = page.getByTestId("web-proposal");
  await expect(proposal.getByRole("textbox", { name: "Search query" })).toHaveValue("Einwohnerzahl Oslo", { timeout: 20_000 });
  await proposal.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page.getByTestId("assistant-answer")).toContainText("728.714", { timeout: 20_000 });
  const sources = page.getByTestId("web-sources");
  await expect(sources).toContainText("Searched the web for “Einwohnerzahl Oslo” · Wikipedia");
  await expect(sources.getByRole("link", { name: "Oslo" })).toHaveAttribute("href", /\/wiki\/Oslo$/);
  await expect(proposal).toHaveCount(0);
});
