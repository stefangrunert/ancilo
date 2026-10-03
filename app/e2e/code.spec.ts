import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";
import type { Page } from "@playwright/test";
import { expect, project, test, waitStatus, type Daemon } from "./harness";

test.use({ daemonArgs: ["--coding"] });

const gitStatus = (dir: string) => execFileSync("git", ["status", "--porcelain", "--untracked-files=all"], { cwd: dir }).toString();

/** Opens the code view with a project and a fresh session on Dev. */
async function start(page: Page, daemon: Daemon): Promise<string> {
  await waitStatus(daemon, "alt-q8_0", "ready");
  const dir = project();
  // Diffs, terminal and variants are the expert view.
  await daemon.op("set_preferences", { view: "pro" });
  await daemon.open(page);
  await page.getByRole("button", { name: "Add project" }).click();
  await page.getByText("For experts: type a folder path").click();
  await page.getByRole("textbox", { name: "Project folder" }).fill(dir);
  await page.getByRole("button", { name: "Open", exact: true }).click();
  await page.getByRole("button", { name: "New session" }).click();
  await expect(page.getByTestId("session")).toBeVisible();
  await page.getByRole("combobox", { name: "Model" }).selectOption("dev-q8_0");
  await expect.poll(async () => (await daemon.op("list_sessions", {}))[0].model).toBe("dev-q8_0");
  return dir;
}

/** "Ask first": the agent asks before every change and command. */
async function askFirst(page: Page, daemon: Daemon) {
  const mode = page.getByRole("combobox", { name: "Access" });
  // New sessions start with "Approve for me".
  await expect(mode).toHaveValue("shell");
  await mode.selectOption("read");
  await expect.poll(async () => (await daemon.op("list_sessions", {}))[0].permission).toBe("read");
}

async function send(page: Page, text: string) {
  const box = page.getByRole("textbox", { name: /What should the agent do/ });
  await box.fill(text);
  await box.press("Enter");
}

// covers: M8-AC-01
test("a task is done entirely in the chat: ask, approve, review the diff, apply", async ({ page, daemon }) => {
  const dir = await start(page, daemon);
  await askFirst(page, daemon);
  await send(page, "Greet in the README and build");
  // "Ask first": the change and the command each wait for an OK.
  const approval = page.getByTestId("approval");
  await expect(approval).toContainText("edit_file README.md", { timeout: 20_000 });
  await approval.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(approval).toContainText("$ echo ok > build.txt", { timeout: 20_000 });
  await approval.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(page.getByTestId("messages")).toContainText("Updated README.md and ran the build.");
  const changes = page.getByTestId("changes");
  await expect(changes).toContainText("README.md");
  await expect(changes).toContainText("build.txt");
  // Nothing reached the project yet.
  expect(gitStatus(dir)).toBe("");
  await changes.getByRole("button", { name: "README.md" }).click();
  await expect(page.getByTestId("patch")).toContainText("+Hello from Dev.");
  await changes.getByRole("button", { name: "Apply all" }).click();
  await expect(page.getByTestId("no-changes")).toBeVisible();
  expect(readFileSync(join(dir, "README.md"), "utf8")).toContain("Hello from Dev.");
  expect(readFileSync(join(dir, "build.txt"), "utf8").trim()).toBe("ok");
  // The session is listed and keeps its conversation after a reload.
  await page.reload();
  await expect(page.getByTestId("messages")).toContainText("Updated README.md and ran the build.");
});

// covers: M8-AC-02
test("rejected and discarded changes leave no trace", async ({ page, daemon }) => {
  const dir = await start(page, daemon);
  await askFirst(page, daemon);
  await send(page, "Greet in the README and build");
  const approval = page.getByTestId("approval");
  await expect(approval).toContainText("edit_file README.md", { timeout: 20_000 });
  await approval.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(approval).toContainText("$ echo ok", { timeout: 20_000 });
  await approval.getByRole("button", { name: "Reject" }).click();
  await expect(page.getByTestId("messages")).toContainText("Updated README.md and ran the build.");
  const changes = page.getByTestId("changes");
  await expect(changes).toContainText("README.md");
  await expect(changes).not.toContainText("build.txt");
  await changes.getByRole("button", { name: "Discard all" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Discard" }).click();
  await expect(page.getByTestId("no-changes")).toBeVisible();
  expect(gitStatus(dir)).toBe("");
  expect(existsSync(join(dir, "build.txt"))).toBe(false);
  expect(readFileSync(join(dir, "README.md"), "utf8")).toBe("# Demo\n");
});

// covers: M8-AC-05
test("retry with another model: same start, side by side, one is taken", async ({ page, daemon }) => {
  const dir = await start(page, daemon);
  // "Approve for me" (the default): no questions.
  await expect(page.getByRole("combobox", { name: "Access" })).toHaveValue("shell");
  await send(page, "Greet in the README and build");
  await expect(page.getByTestId("messages")).toContainText("Updated README.md and ran the build.", { timeout: 20_000 });
  await page.getByRole("combobox", { name: "Try again with" }).selectOption("alt-q8_0");
  await page.getByRole("button", { name: "Retry" }).click();
  const alt = page.getByTestId("variant-alt-q8_0");
  await expect(alt).toContainText("Alt renamed the heading.", { timeout: 20_000 });
  // Both results side by side; Alt started from the same state (it saw "# Demo").
  const variants = page.getByTestId("variants");
  await expect(variants).toContainText("+Hello from Dev.");
  await expect(alt).toContainText("+# Demo (by Alt)");
  await expect(alt).not.toContainText("Hello from Dev.");
  expect(gitStatus(dir)).toBe("");
  await alt.getByRole("button", { name: "Use this result" }).click();
  await expect(page.getByTestId("no-changes")).toBeVisible();
  expect(readFileSync(join(dir, "README.md"), "utf8")).toBe("# Demo (by Alt)\n");
  expect(existsSync(join(dir, "build.txt"))).toBe(false);
  await expect(page.getByTestId("messages")).toContainText("Alt renamed the heading.");
});

async function terminalText(page: Page, id: string): Promise<string> {
  return (await page.getByTestId(`terminal-${id}`).locator(".xterm-rows").textContent()) ?? "";
}

// covers: M8-AC-04
test("the terminal works, resizes, survives a reload, and there can be several", async ({ page, daemon }) => {
  const dir = await start(page, daemon);
  await page.getByRole("tab", { name: "Terminal" }).click();
  await page.getByRole("button", { name: "New terminal" }).click();
  const term = page.locator("[data-testid^=terminal-]");
  await expect(term).toHaveAttribute("data-state", "open");
  const id = ((await term.getAttribute("data-testid")) ?? "").replace("terminal-", "");
  await term.locator(".xterm").click();
  await page.keyboard.type("echo marker-$((6*7))");
  await page.keyboard.press("Enter");
  await expect.poll(() => terminalText(page, id)).toContain("marker-42");
  // It opens in the project folder.
  await page.keyboard.type("pwd");
  await page.keyboard.press("Enter");
  await expect.poll(() => terminalText(page, id)).toMatch(new RegExp(`pwd/\\S*/${basename(dir)}`));
  expect(await terminalText(page, id)).not.toContain("/sessions/");

  // Resizing the window resizes the terminal.
  await page.keyboard.type("stty size");
  await page.keyboard.press("Enter");
  await expect.poll(() => terminalText(page, id)).toMatch(/\d+ \d+/);
  const before = (await terminalText(page, id)).match(/(\d+) (\d+)\s*\S*\s*$/m);
  await page.setViewportSize({ width: 1500, height: 900 });
  await page.waitForTimeout(300);
  await term.locator(".xterm").click();
  await page.keyboard.type("clear; stty size");
  await page.keyboard.press("Enter");
  await expect
    .poll(async () => {
      const m = (await terminalText(page, id)).match(/(\d+) (\d+)/);
      return m ? `${m[1]} ${m[2]}` : "";
    })
    .not.toBe(before ? `${before[1]} ${before[2]}` : "");

  // The terminal lives in Ancilo: a reload reconnects with its output.
  await page.keyboard.type("echo still-$((40+2))");
  await page.keyboard.press("Enter");
  await expect.poll(() => terminalText(page, id)).toContain("still-42");
  await page.reload();
  await page.getByRole("tab", { name: "Terminal" }).click();
  await expect(page.getByTestId(`terminal-${id}`)).toHaveAttribute("data-state", "open");
  await expect.poll(() => terminalText(page, id)).toContain("still-42");

  // A second terminal, independent of the first.
  await page.getByRole("button", { name: "New terminal" }).click();
  await expect(page.getByRole("tab", { name: "Terminal 2" })).toBeVisible();
  const second = page.locator("[data-testid^=terminal-]");
  await expect(second).toHaveAttribute("data-state", "open");
  const id2 = ((await second.getAttribute("data-testid")) ?? "").replace("terminal-", "");
  expect(id2).not.toBe(id);
  expect(await terminalText(page, id2)).not.toContain("still-42");
  expect((await daemon.op("list_terminals")).length).toBe(2);
  await page.getByRole("button", { name: "Close terminal 2" }).click();
  await expect(page.getByRole("tab", { name: "Terminal 2" })).toHaveCount(0);
  expect((await daemon.op("list_terminals")).length).toBe(1);
});

// covers: M7-AC-07 (the code view too)
test("the code view has no serious accessibility findings", async ({ page, daemon }) => {
  const { default: AxeBuilder } = await import("@axe-core/playwright");
  await start(page, daemon);
  await askFirst(page, daemon);
  await send(page, "Greet in the README and build");
  await expect(page.getByTestId("approval")).toBeVisible({ timeout: 20_000 });
  const check = async () => {
    const scan = await new AxeBuilder({ page }).analyze();
    const serious = scan.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
    expect(serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  };
  await check();
  await page.getByRole("tab", { name: "Terminal" }).click();
  await page.getByRole("button", { name: "New terminal" }).click();
  await expect(page.locator("[data-testid^=terminal-]")).toHaveAttribute("data-state", "open");
  await check();
});
