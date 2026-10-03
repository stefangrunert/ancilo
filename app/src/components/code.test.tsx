import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { model, renderWithDaemon, type Handler } from "../test-utils";
import { blocks, Patch, ProjectView, SessionView } from "./Code";

vi.mock("./Terminal", () => ({ TerminalView: ({ id }: { id: string }) => <div data-testid={`terminal-${id}`} /> }));

type Models = Parameters<typeof SessionView>[0]["models"];
const models = [model("dev"), model("alt"), model("embed", { embedding: true })] as unknown as Models;

function session(extra: Record<string, unknown> = {}) {
  return {
    id: "s-1",
    title: "Rename things",
    project: "/p/wcs",
    model: "dev",
    permission: "shell",
    status: "idle",
    isolated: true,
    workdir: "/home/sessions/s-1/work",
    turns: 1,
    changes: [],
    approvals: [],
    variants: [],
    can_retry: false,
    web_used: false,
    messages: [],
    created_at: "",
    ...extra,
  };
}

function daemon(s: ReturnType<typeof session>, more: Record<string, Handler> = {}) {
  return renderWithDaemon(<SessionView id="s-1" models={models} />, {
    // The expert view (diffs, terminal, variants); the simple one is tested below.
    get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }),
    get_session: () => s,
    session_diff: () => ({ files: s.changes, patch: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n" }),
    list_terminals: () => [],
    ...more,
  });
}

beforeAll(() => {
  // Wide enough for the changes panel beside the conversation.
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
});

describe("Patch", () => {
  it("marks added, removed and hunk lines", () => {
    render(<Patch text={"@@ -1 +1 @@\n-old\n+new\n same"} />);
    expect(screen.getByText("+new")).toHaveClass("add");
    expect(screen.getByText("-old")).toHaveClass("del");
    expect(screen.getByText("@@ -1 +1 @@")).toHaveClass("hunk");
  });
});

describe("blocks", () => {
  it("folds tool calls and their results into steps between what was said", () => {
    expect(
      blocks([
        { role: "user", text: "Rename old" },
        { role: "assistant", text: "", tool_calls: ["read_file({})", "grep({})"] },
        { role: "tool", text: "a" },
        { role: "tool", text: "b" },
        { role: "assistant", text: "Found it.", tool_calls: ["edit_file({})"] },
        { role: "tool", text: "ok" },
        { role: "assistant", text: "Done." },
      ]),
    ).toEqual([
      { kind: "user", text: "Rename old" },
      { kind: "steps", steps: [{ call: "read_file({})", result: "a" }, { call: "grep({})", result: "b" }] },
      { kind: "say", text: "Found it." },
      { kind: "steps", steps: [{ call: "edit_file({})", result: "ok" }] },
      { kind: "say", text: "Done." },
    ]);
  });
});

// covers: M8-AC-03
describe("SessionView", () => {
  it("shows the conversation: the answer as Markdown, the steps folded", async () => {
    daemon(
      session({
        messages: [
          { role: "user", text: "Rename old" },
          { role: "assistant", text: "", tool_calls: ["read_file({\"path\":\"src/lib.rs\"})"] },
          { role: "tool", text: "1 pub fn old()" },
          { role: "assistant", text: "Renamed `old` to **new**." },
        ],
      }),
    );
    const messages = await screen.findByTestId("messages");
    expect(within(messages).getByText("new").tagName).toBe("STRONG");
    expect(within(messages).getByText("old").tagName).toBe("CODE");
    expect(within(messages).getByText("1 steps")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Rename things" })).toBeInTheDocument();
    // Where the work happens is said plainly.
    expect(screen.getByRole("button", { name: "wcs" })).toBeInTheDocument();
  });

  it("asks before actions above the permission and sends exactly the decision", async () => {
    const s = session({
      status: "running",
      approvals: [{ id: "ap-1", session: "s-1", tool: "bash", arguments: { command: "cargo test" }, needs: "shell", created_at: "" }],
    });
    const { calls } = daemon(s, { approve: () => ({}), reject: () => ({}), cancel_turn: () => ({}) });
    const card = await screen.findByTestId("approval");
    expect(card).toHaveTextContent("$ cargo test");
    await userEvent.click(within(card).getByRole("button", { name: "Allow for this session" }));
    await waitFor(() => expect(calls.find((c) => c.op === "approve")?.input).toEqual({ approval: "ap-1", remember: true }));
    await userEvent.click(within(card).getByRole("button", { name: "Reject" }));
    await waitFor(() => expect(calls.find((c) => c.op === "reject")?.input).toEqual({ approval: "ap-1" }));
    // While running, the turn can be stopped but no new message sent.
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(calls.some((c) => c.op === "cancel_turn")).toBe(true));
  });

  // covers: M8-AC-13
  it("asks before every web search, only for this once, and says what goes out", async () => {
    const s = session({
      status: "running",
      approvals: [{ id: "ap-2", session: "s-1", tool: "web_search", arguments: { query: "tokio select" }, needs: "shell", sends_to: "wikipedia", created_at: "" }],
    });
    const { calls } = daemon(s, { approve: () => ({}), reject: () => ({}) });
    const card = await screen.findByTestId("approval");
    expect(card).toHaveTextContent("The agent wants to search Wikipedia for: tokio select");
    expect(card).toHaveTextContent("These search words go to Wikipedia.");
    expect(within(card).queryByRole("button", { name: "Allow for this session" })).toBeNull();
    await userEvent.click(within(card).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(calls.find((c) => c.op === "approve")?.input).toEqual({ approval: "ap-2", remember: false }));
    await userEvent.click(within(card).getByRole("button", { name: "Don’t search" }));
    await waitFor(() => expect(calls.find((c) => c.op === "reject")?.input).toEqual({ approval: "ap-2" }));
  });

  // covers: M8-AC-13
  it("points to changes that run on build and to web text before keeping", async () => {
    const s = session({ web_used: true, changes: [{ path: "build.rs", added: 3, removed: 0, runs: true }, { path: "src/a.rs", added: 1, removed: 0, runs: false }] });
    daemon(s, {});
    const changes = await screen.findByTestId("changes");
    expect(within(changes).getAllByText("runs code")).toHaveLength(1);
    const note = within(screen.getByTestId("changes-card")).getByTestId("keep-note");
    expect(note).toHaveTextContent("Some changes run when the project is built, installed or tested (build.rs).");
    expect(note).toHaveTextContent("The agent read web pages in this session.");
  });

  it("sends multi-line messages with Enter", async () => {
    const { calls } = daemon(session(), { send_message: () => session({ status: "running" }), list_sessions: () => [], list_projects: () => [] });
    const box = await screen.findByRole("textbox", { name: "What should the agent do?" });
    await userEvent.type(box, "first{Shift>}{Enter}{/Shift}second{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "send_message")?.input).toEqual({ session: "s-1", text: "first\nsecond" }));
  });

  it("applies from the chat after a turn", async () => {
    const s = session({ changes: [{ path: "a.rs", added: 3, removed: 1 }] });
    const { calls } = daemon(s, { apply_changes: () => ({ files: ["a.rs"] }) });
    const card = await screen.findByTestId("changes-card");
    expect(card).toHaveTextContent("1 file(s) changed");
    expect(card).toHaveTextContent("+3");
    await userEvent.click(within(card).getByRole("button", { name: "Apply to project" }));
    await waitFor(() => expect(calls.find((c) => c.op === "apply_changes")).toMatchObject({ input: { session: "s-1", paths: null }, confirmed: true }));
  });

  it("applies with confirmation, discards only after asking, per file or all", async () => {
    const s = session({ changes: [{ path: "a.rs", added: 1, removed: 1 }, { path: "b.rs", added: 2, removed: 0 }] });
    const { calls } = daemon(s, { apply_changes: () => ({ files: ["a.rs"] }), discard_changes: () => ({ files: ["b.rs"] }) });
    await screen.findByTestId("changes");
    await userEvent.click(screen.getByRole("checkbox", { name: "Select a.rs" }));
    await userEvent.click(screen.getByRole("button", { name: "Apply 1 selected" }));
    await waitFor(() => expect(calls.find((c) => c.op === "apply_changes")).toMatchObject({ input: { session: "s-1", paths: ["a.rs"] }, confirmed: true }));
    await userEvent.click(screen.getByRole("button", { name: "Discard 1 selected" }));
    expect(calls.some((c) => c.op === "discard_changes")).toBe(false);
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(calls.find((c) => c.op === "discard_changes")?.input).toEqual({ session: "s-1", paths: ["a.rs"] }));
  });

  it("offers a retry with another local model and shows variants side by side", async () => {
    const s = session({
      can_retry: true,
      changes: [{ path: "a.rs", added: 1, removed: 1 }],
      variants: [{ id: "v-1", model: "alt", status: "idle", summary: "Alt did it.", changes: [{ path: "a.rs", added: 1, removed: 0 }] }],
    });
    const { calls } = daemon(s, { retry_with_model: () => s, apply_changes: () => ({ files: ["a.rs"] }) });
    const retryWith = await screen.findByRole("combobox", { name: "Try again with" });
    // Only other chat models: not the current one, no embedding models.
    expect(within(retryWith).getAllByRole("option").map((o) => o.textContent)).toEqual(["alt"]);
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(calls.find((c) => c.op === "retry_with_model")?.input).toEqual({ session: "s-1", model: "alt" }));
    const v = screen.getByTestId("variant-alt");
    expect(v).toHaveTextContent("Alt did it.");
    await userEvent.click(within(v).getByRole("button", { name: "Use this result" }));
    await waitFor(() => expect(calls.find((c) => c.op === "apply_changes")).toMatchObject({ input: { session: "s-1", variant: "v-1" }, confirmed: true }));
  });
});

describe("SessionView, simple", () => {
  it("offers keep or undo and the two access modes – no diff or terminal", async () => {
    const s = session({
      changes: [{ path: "a.rs", added: 3, removed: 1 }],
      messages: [{ role: "assistant", text: "", tool_calls: ["read_file({})"] }, { role: "tool", text: "x" }, { role: "assistant", text: "Done." }],
    });
    const { calls } = renderWithDaemon(<SessionView id="s-1" models={models} />, {
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      get_session: () => s,
      list_terminals: () => [],
      discard_changes: () => ({ files: ["a.rs"] }),
      apply_changes: () => ({ files: ["a.rs"] }),
      update_session: () => s,
      list_sessions: () => [],
    });
    const card = await screen.findByTestId("changes-card");
    expect(screen.getByText("Ancilo worked on it (1 steps).")).toBeInTheDocument();
    // The access modes are for everyone: "Confirm each step" or "Work on its own".
    const mode = screen.getByRole("combobox", { name: "Access" });
    expect(within(mode).getAllByRole("option").map((o) => o.textContent)).toEqual(["Confirm each step", "Work on its own"]);
    await userEvent.selectOptions(mode, "read");
    await waitFor(() => expect(calls.find((c) => c.op === "update_session")?.input).toEqual({ session: "s-1", permission: "read" }));
    expect(screen.queryByRole("tab", { name: "Terminal" })).toBeNull();
    expect(screen.queryByTestId("changes")).toBeNull();
    await userEvent.click(within(card).getByRole("button", { name: "Undo" }));
    expect(calls.some((c) => c.op === "discard_changes")).toBe(false);
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(calls.find((c) => c.op === "discard_changes")?.input).toEqual({ session: "s-1", paths: null }));
    await userEvent.click(within(card).getByRole("button", { name: "Keep" }));
    await waitFor(() => expect(calls.find((c) => c.op === "apply_changes")).toMatchObject({ input: { session: "s-1", paths: null }, confirmed: true }));
  });
});

describe("ProjectView", () => {
  afterEach(() => window.history.replaceState(null, "", "#/"));

  it("starts a session with the first task and opens it", async () => {
    const { calls } = renderWithDaemon(<ProjectView root="/p/wcs" models={models} />, {
      open_project: () => ({ root: "/p/wcs", name: "wcs", git: true, sessions: [] }),
      list_sessions: () => [session({ title: "Older" })],
      list_projects: () => [],
      create_session: () => session({ id: "s-2" }),
      send_message: () => session({ id: "s-2", status: "running" }),
    });
    expect(await screen.findByRole("heading", { name: "wcs" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /Older/ })).toBeInTheDocument();
    await userEvent.type(screen.getByRole("textbox", { name: "What should the agent do?" }), "Add a README{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "create_session")?.input).toEqual({ cwd: "/p/wcs" }));
    await waitFor(() => expect(calls.find((c) => c.op === "send_message")?.input).toEqual({ session: "s-2", text: "Add a README" }));
    await waitFor(() => expect(window.location.hash).toBe("#/session/s-2"));
  });
});
