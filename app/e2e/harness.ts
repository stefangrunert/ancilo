import { test as base, expect, type Page } from "@playwright/test";
import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const root = new URL("../..", import.meta.url).pathname;

export interface Daemon {
  url: string;
  token: string;
  home: string;
  /** The computer's state the daemon sees (free memory, pressure, heat, programs). */
  probe: string;
  proc: ChildProcess;
  /** Calls an operation directly (like the CLI would). */
  op: (name: string, input?: unknown, confirm?: boolean) => Promise<any>;
  /** Opens the app (its start page: Set up). */
  open: (page: Page) => Promise<void>;
  /** Opens the System page – in the expert view unless `pro` is false. */
  openSystem: (page: Page, pro?: boolean) => Promise<void>;
  kill: () => void;
}

export async function startDaemon(args: string[] = []): Promise<Daemon> {
  const proc = spawn(join(root, "target/debug/ancilo-e2e"), args, { stdio: ["pipe", "pipe", "inherit"] });
  const line = await new Promise<string>((resolve, reject) => {
    const rl = createInterface({ input: proc.stdout! });
    rl.once("line", resolve);
    proc.once("exit", (code) => reject(new Error(`harness exited with ${code}`)));
  });
  const info = JSON.parse(line) as { url: string; token: string; home: string; probe: string };
  const op = async (name: string, input: unknown = {}, confirm = false) => {
    const r = await fetch(`${info.url}/api/v1/ops/${name}`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${info.token}`, ...(confirm ? { "x-ancilo-confirm": "true" } : {}) },
      body: JSON.stringify(input),
    });
    const body = await r.json();
    if (!r.ok) throw new Error(`${name}: ${JSON.stringify(body)}`);
    return body;
  };
  return {
    ...info,
    proc,
    op,
    open: async (page) => {
      await page.goto(`${info.url}/app/#token=${info.token}`);
      await expect(page.getByTestId("setup")).toBeVisible();
    },
    openSystem: async (page, pro = true) => {
      await op("set_preferences", { view: pro ? "pro" : "simple" });
      await page.goto(`${info.url}/app/#token=${info.token}`);
      await page.getByRole("button", { name: "System", exact: true }).click();
      await expect(page.getByRole("heading", { name: "System", exact: true })).toBeVisible();
    },
    kill: () => {
      proc.stdin?.end();
      proc.kill("SIGTERM");
    },
  };
}

/** Waits until a model has the status. */
export async function waitStatus(d: Daemon, model: string, status: string) {
  await expect
    .poll(async () => (await d.op("model_status", { model })).status, { timeout: 20_000 })
    .toBe(status);
}

/** A small git project for comparisons. */
export function project(): string {
  const dir = mkdtempSync(join(tmpdir(), "ancilo-e2e-"));
  writeFileSync(join(dir, "README.md"), "# Demo\n");
  const git = (...a: string[]) =>
    execFileSync("git", a, { cwd: dir, env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@x", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@x" } });
  git("init", "-q", "-b", "main");
  git("add", "-A");
  git("commit", "-q", "-m", "init");
  return dir;
}

export const test = base.extend<{ daemon: Daemon; daemonArgs: string[] }>({
  daemonArgs: [[], { option: true }],
  daemon: async ({ daemonArgs }, use) => {
    const d = await startDaemon(daemonArgs);
    await use(d);
    d.kill();
  },
});

export { expect };
