import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { model, renderWithDaemon, type Handler } from "../test-utils";
import { AddModel } from "./AddModel";
import { Markdown } from "./Markdown";
import { Connect } from "./Settings";

vi.mock("./Terminal", () => ({ TerminalView: ({ id }: { id: string }) => <div data-testid={`terminal-${id}`} /> }));

const base: Record<string, Handler> = {
  hardware_info: () => ({ model_budget_bytes: 64 << 30, used_bytes: 1 << 30, free_for_models_bytes: 63 << 30, hardware: {} }),
  get_permissions: () => ({ max_access: "shell" }),
  connections: () => [
    { client: "claude_code", connected: false, verified: false, since: null },
    { client: "codex", connected: false, verified: false, since: null },
  ],
  list_tasks: () => [],
  list_comparisons: () => [],
  ab_status: () => [],
  recommendations: () => [],
  list_models: () => [model("chat", { roles: ["default"], status: "running" })],
  list_projects: () => [],
  list_sessions: () => [],
  list_conversations: () => [],
  pending_actions: () => [],
  get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }),
  resource_status: () => ({ settings: { level: "balanced", custom: false, keep_loaded_secs: 900, max_share: 0.55, parallel: 2, priority: "low", variant: "auto", guard: "unload_when_tight" }, presets: [], system: { available_bytes: 0, pressure: "normal", thermal: "nominal", swap_used_bytes: 0 }, total_bytes: 1, cap_bytes: 1, used_bytes: 0, loaded: [], recent: [] }),
};

const at = (hash: string) => window.history.replaceState(null, "", hash);

const ANSWER = "I propose to use **coder** for delegation:\n\n- faster\n- tested\n\n```sh\nancilo list\n```\n\n<img src=x onerror=alert(1)><script>alert(2)</script>";

function conversationOps(state: { messages: Record<string, unknown>[]; live: boolean }): Record<string, Handler> {
  const pending = { id: "a-1", operation: "assign_role", input: { role: "delegation", model: "coder" }, summary: "Use a model for a role", consequential: false, created_at: "" };
  return {
    ...base,
    ask: (i) => {
      state.messages.push({ role: "user", text: String(i.prompt), at: "" }, { role: "assistant", text: ANSWER, at: "", model: "chat", pending: [pending] });
      return { answer: ANSWER, model: "chat", steps: 2, operations: [], pending: [pending], conversation: "c-1" };
    },
    get_conversation: () => ({ id: "c-1", title: "use coder", created_at: "", updated_at: "", messages: state.messages }),
    list_conversations: () => (state.messages.length ? [{ id: "c-1", title: "use coder", created_at: "", updated_at: "", messages: state.messages.length }] : []),
    pending_actions: () => (state.live ? [pending] : []),
    confirm_action: () => {
      state.live = false;
      (state.messages[1]!.pending as Record<string, unknown>[])[0]!.outcome = "executed";
      return { action: pending, result: {} };
    },
  };
}

// covers: M7-AC-10, M7-AC-03
describe("Conversations with the assistant", () => {
  afterEach(() => at("#/"));

  it("are a dialog: multi-line input, the answer as Markdown, and exactly the confirmed action runs", async () => {
    at("#/chat/new");
    const state = { messages: [] as Record<string, unknown>[], live: true };
    const { calls } = renderWithDaemon(<App />, conversationOps(state));
    const box = await screen.findByRole("textbox", { name: "What should Ancilo do?" });
    await userEvent.type(box, "use coder{Shift>}{Enter}{/Shift}for delegation");
    expect(calls.some((c) => c.op === "ask")).toBe(false);
    await userEvent.type(box, "{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "ask")?.input).toEqual({ prompt: "use coder\nfor delegation", remember: true, kind: "chat", greeting: null }));
    // The new conversation opens and is listed.
    await waitFor(() => expect(window.location.hash).toBe("#/chat/c-1"));
    const answer = await screen.findByTestId("assistant-answer");
    expect(screen.getByTestId("user-message")).toHaveTextContent("use coder for delegation");
    expect(within(answer).getByText("coder").tagName).toBe("STRONG");
    expect(within(answer).getAllByRole("listitem")).toHaveLength(2);
    expect(within(answer).getByText("ancilo list").closest("pre")).not.toBeNull();
    expect(within(answer).getByRole("button", { name: "Copy" })).toBeInTheDocument();
    // Model output never becomes markup.
    expect(answer.querySelector("img, script")).toBeNull();
    expect(await screen.findByRole("button", { name: "use coder" })).toBeInTheDocument();
    // The proposal, in plain words: runs only after the click, exactly as proposed.
    const proposal = screen.getByRole("group", { name: "Ancilo proposes" });
    expect(proposal).toHaveTextContent("Shall I do this?");
    expect(proposal).toHaveTextContent("From now on coder takes care of the Coding Tasks Claude Code or Codex hand over.");
    // The technical form only folded away.
    expect(within(proposal).getByText("assign_role role=delegation model=coder")).not.toBeVisible();
    await userEvent.click(within(proposal).getByRole("button", { name: "Yes, do it" }));
    const confirm = calls.find((c) => c.op === "confirm_action")!;
    expect(confirm.input).toEqual({ id: "a-1" });
    expect(confirm.confirmed).toBe(true);
    expect(await within(proposal).findByText("Done.")).toBeInTheDocument();
  });

  it("continue in the same conversation", async () => {
    at("#/chat/c-1");
    const state = { messages: [{ role: "user", text: "first", at: "" }, { role: "assistant", text: "first answer", at: "" }] as Record<string, unknown>[], live: false };
    const { calls } = renderWithDaemon(<App />, conversationOps(state));
    expect(await screen.findByText("first answer")).toBeInTheDocument();
    await userEvent.type(screen.getByRole("textbox", { name: "What should Ancilo do?" }), "and then?{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "ask")?.input).toEqual({ prompt: "and then?", conversation: "c-1" }));
  });

  it("start from \"New chat\" – the system page has no chat of its own", async () => {
    at("#/system");
    const state = { messages: [] as Record<string, unknown>[], live: false };
    const { calls } = renderWithDaemon(<App />, conversationOps(state));
    expect(await screen.findByRole("heading", { name: "System" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "What should Ancilo do?" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "New chat" }));
    await userEvent.type(await screen.findByRole("textbox", { name: "What should Ancilo do?" }), "hello{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "ask")?.input).toEqual({ prompt: "hello", remember: true, kind: "chat", greeting: null }));
    await waitFor(() => expect(window.location.hash).toBe("#/chat/c-1"));
  });
});

// covers: M6-AC-12
describe("Quick actions", () => {
  afterEach(() => at("#/"));

  it("offer just chatting (chosen), building and setting Ancilo up – with a way back", async () => {
    at("#/chat/new");
    const state = { messages: [] as Record<string, unknown>[], live: false };
    const { calls } = renderWithDaemon(<App />, conversationOps(state));
    const actions = await screen.findByRole("navigation", { name: "What would you like to do?" });
    expect(within(actions).getAllByRole("button").map((b) => b.textContent)).toEqual(["Just chat", "Build something", "Set up Ancilo"]);
    expect(within(actions).getByRole("button", { name: "Just chat" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(within(actions).getByRole("button", { name: "Set up Ancilo" }));
    expect(window.location.hash).toBe("#/chat/new/setup");
    expect(await screen.findByTestId("greeting")).toHaveTextContent("Hi, I'm Ancilo – I'll help you set me up.");
    // No request yet – the greeting is instant.
    expect(calls.some((c) => c.op === "ask")).toBe(false);
    await userEvent.click(screen.getByRole("button", { name: "My computer is getting slow" }));
    await waitFor(() =>
      expect(calls.find((c) => c.op === "ask")?.input).toEqual({
        prompt: "My computer is getting slow",
        remember: true,
        kind: "setup",
        greeting: "Hi, I'm Ancilo – I'll help you set me up. Where shall we start?",
      }),
    );
  });

  it("lead back to a new chat from setting up and from building", async () => {
    for (const where of ["#/chat/new/setup", "#/build"]) {
      at(where);
      const view = renderWithDaemon(<App />, conversationOps({ messages: [], live: false }));
      await userEvent.click(await screen.findByRole("button", { name: "Back to new chat" }));
      expect(window.location.hash).toBe("#/chat/new");
      expect(await screen.findByRole("heading", { name: "Ancilo Chat" })).toBeInTheDocument();
      view.unmount();
    }
  });

  it("build something: a name, then the folder it goes into", async () => {
    at("#/build");
    const { calls } = renderWithDaemon(<App />, {
      ...base,
      choose_folder: () => ({ path: "/Users/me/www" }),
      create_project: (i) => ({ root: `${String(i.parent)}/${String(i.name)}`, name: String(i.name), git: true, sessions: [] }),
      open_project: (i) => ({ root: i.path, name: "Recipes", git: true, sessions: [] }),
    });
    await userEvent.type(await screen.findByRole("textbox", { name: "New project – what should it be called?" }), "Recipes");
    // Nothing is created before the folder is chosen.
    expect(screen.getByRole("button", { name: "Create project" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Choose where it goes…" }));
    expect(await screen.findByTestId("project-location")).toHaveTextContent("Ancilo creates /Users/me/www/Recipes");
    expect(calls.find((c) => c.op === "choose_folder")?.input).toEqual({ prompt: "Where should the folder for “Recipes” go?" });
    await userEvent.click(screen.getByRole("button", { name: "Create project" }));
    await waitFor(() => expect(calls.find((c) => c.op === "create_project")?.input).toEqual({ name: "Recipes", parent: "/Users/me/www" }));
    await waitFor(() => expect(window.location.hash).toBe("#/project/%2FUsers%2Fme%2Fwww%2FRecipes"));
  });
});

describe("Layout", () => {
  afterEach(() => at("#/"));

  // covers: M10-AC-01
  it("has set up and system in the header and the areas on top of the left column", async () => {
    const simple = { view: "simple", purposes: ["chat"], setup: {}, documents: [] };
    const { calls } = renderWithDaemon(<App />, { ...base, get_preferences: () => simple });
    await waitFor(() => expect(calls.some((c) => c.op === "get_preferences")).toBe(true));
    const header = screen.getByRole("banner");
    expect(within(header).getAllByRole("button").map((b) => b.textContent)).toEqual(["", "Set up", "System"]);
    const nav = screen.getByRole("navigation", { name: "Navigation" });
    // Code only for those who program: not chosen in the setup, no projects, simple view.
    const tabs = within(nav).getByRole("tablist", { name: "Areas" });
    expect(within(tabs).getAllByRole("tab").map((b) => b.textContent)).toEqual(["Chat", "Tasks"]);
    expect(within(tabs).getByRole("tab", { name: "Chat" })).toHaveAttribute("aria-selected", "true");
    expect(within(nav).getByRole("tabpanel")).toHaveTextContent("New chat");
    // A clean sidebar: settings live in the status bar.
    expect(within(nav).queryByRole("switch")).toBeNull();
    expect(within(nav).queryByRole("combobox")).toBeNull();
    // Arrow keys move between the areas; the page follows.
    within(tabs).getByRole("tab", { name: "Chat" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    await waitFor(() => expect(window.location.hash).toBe("#/tasks"));
    expect(within(tabs).getByRole("tab", { name: "Tasks" })).toHaveAttribute("aria-selected", "true");
    expect(await screen.findByTestId("tasks-area")).toBeInTheDocument();
    // Back to the chat area: it opens where it was.
    await userEvent.click(within(tabs).getByRole("tab", { name: "Chat" }));
    await waitFor(() => expect(window.location.hash).toBe("#/chat/new"));
  });

  // covers: M10-AC-01
  it("lets the left column be resized by the keyboard, within its limits, and reset by a double click", async () => {
    renderWithDaemon(<App />, base);
    const handle = screen.getByRole("separator", { name: "Width of the left column" });
    expect(handle).toHaveAttribute("aria-valuenow", "264");
    handle.focus();
    await userEvent.keyboard("{ArrowRight}{ArrowRight}");
    expect(handle).toHaveAttribute("aria-valuenow", "284");
    await userEvent.keyboard("{End}");
    expect(handle).toHaveAttribute("aria-valuenow", "420");
    await userEvent.dblClick(handle);
    expect(handle).toHaveAttribute("aria-valuenow", "264");
  });

  it("shows the Code area for programmers, with the Coding Tasks in the expert view", async () => {
    renderWithDaemon(<App />, { ...base, get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }) });
    const nav = screen.getByRole("navigation", { name: "Navigation" });
    await userEvent.click(await within(nav).findByRole("tab", { name: "Code" }));
    await userEvent.click(within(nav).getByRole("button", { name: "Coding Tasks" }));
    expect(await screen.findByRole("heading", { name: "Coding Tasks" })).toBeInTheDocument();
    expect(screen.getByText("No Coding Tasks yet.")).toBeInTheDocument();
  });
});

describe("Markdown", () => {
  it("renders tables and links that leave the app, but no HTML", () => {
    const { container } = renderWithDaemon(<Markdown text={"| a | b |\n|---|---|\n| 1 | 2 |\n\n[docs](https://example.org) <b>bold?</b>"} />, {});
    expect(container.querySelector("table")).not.toBeNull();
    const link = screen.getByRole("link", { name: "docs" });
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
    expect(container.querySelector("b")).toBeNull();
  });
});

describe("Sidebar", () => {
  afterEach(() => at("#/"));

  it("lists projects with their sessions and chats; deleting asks first, renaming saves", async () => {
    at("#/session/s-1");
    const session = { id: "s-1", title: "Rename things", project: "/p/wcs", model: "chat", permission: "edit", status: "idle", isolated: true, workdir: "/w", turns: 1, changes: [], approvals: [], variants: [], can_retry: false, created_at: "" };
    const { calls } = renderWithDaemon(<App />, {
      ...base,
      list_projects: () => [{ root: "/p/wcs", name: "wcs", sessions: 1, last_used: "", exists: true, area: "code" }],
      list_sessions: () => [session],
      get_session: () => ({ ...session, messages: [] }),
      list_terminals: () => [],
      list_conversations: () => [{ id: "c-1", title: "Which models?", created_at: "", updated_at: "", messages: 2 }],
      delete_conversation: () => ({ deleted: true }),
      rename_conversation: (i) => ({ id: "c-1", title: i.title, created_at: "", updated_at: "", messages: 2 }),
    });
    const nav = screen.getByRole("navigation", { name: "Navigation" });
    expect(await within(nav).findByRole("button", { name: "wcs" })).toBeInTheDocument();
    // The open session's project shows its sessions.
    expect(await within(nav).findByRole("button", { name: "Rename things" })).toHaveAttribute("aria-current", "page");
    // The chats are in the chat area.
    await userEvent.click(within(nav).getByRole("tab", { name: "Chat" }));
    await userEvent.click(await within(nav).findByRole("button", { name: "Delete Which models?" }));
    expect(calls.some((c) => c.op === "delete_conversation")).toBe(false);
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Yes" }));
    await waitFor(() => expect(calls.find((c) => c.op === "delete_conversation")?.input).toEqual({ id: "c-1" }));
    await userEvent.click(within(nav).getByRole("button", { name: "Rename Which models?" }));
    const title = within(nav).getByRole("textbox", { name: "New title" });
    await userEvent.clear(title);
    await userEvent.type(title, "Models{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "rename_conversation")?.input).toEqual({ id: "c-1", title: "Models" }));
  });

  // covers: M8-AC-12
  it("renames projects and their sessions and sorts both by hand – dragging or Alt+arrows", async () => {
    at("#/session/s-1");
    const session = (id: string, title: string) => ({ id, title, project: "/p/a", model: "chat", permission: "edit", status: "idle", isolated: true, workdir: "/w", turns: 1, changes: [], approvals: [], variants: [], can_retry: false, created_at: "" });
    const { calls } = renderWithDaemon(<App />, {
      ...base,
      list_projects: () => [
        { root: "/p/a", name: "a", sessions: 2, last_used: "", exists: true, area: "code" },
        { root: "/p/b", name: "b", sessions: 0, last_used: "", exists: true, area: "code" },
      ],
      list_sessions: () => [session("s-1", "One"), session("s-2", "Two")],
      get_session: () => ({ ...session("s-1", "One"), messages: [] }),
      list_terminals: () => [],
      rename_project: () => ({}),
      update_session: () => ({}),
      reorder_projects: () => ({}),
      reorder_sessions: () => ({}),
    });
    const nav = screen.getByRole("navigation", { name: "Navigation" });

    await userEvent.click(await within(nav).findByRole("button", { name: "Rename a" }));
    const name = within(nav).getByRole("textbox", { name: "New title" });
    await userEvent.clear(name);
    await userEvent.type(name, "Alpha{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "rename_project")?.input).toEqual({ path: "/p/a", name: "Alpha" }));

    await userEvent.click(await within(nav).findByRole("button", { name: "Rename One" }));
    const title = within(nav).getByRole("textbox", { name: "New title" });
    await userEvent.clear(title);
    await userEvent.type(title, "First{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "update_session")?.input).toEqual({ session: "s-1", title: "First" }));

    // Keyboard: Alt+↓ moves the session one down and keeps it focused.
    const one = within(nav).getByRole("button", { name: "One" });
    one.focus();
    await userEvent.keyboard("{Alt>}{ArrowDown}{/Alt}");
    await waitFor(() => expect(calls.find((c) => c.op === "reorder_sessions")?.input).toEqual({ sessions: ["s-2", "s-1"] }));
    const sessions = within(nav).getByRole("list", { name: "Sessions in a" });
    expect(within(sessions).getAllByRole("button", { name: /^(One|Two)$/ }).map((b) => b.textContent)).toEqual(["Two", "One"]);

    // Pointer: drag project b above a; the list follows at once, the order is saved once on release.
    const rowA = within(nav).getByRole("button", { name: "a" }).closest("li")!;
    const b = within(nav).getByRole("button", { name: "b" });
    const rowB = b.closest("li")!;
    const rect = (top: number) => () => ({ top, height: 30, bottom: top + 30, left: 0, right: 200, width: 200, x: 0, y: top, toJSON: () => ({}) });
    rowA.getBoundingClientRect = rect(0);
    rowB.getBoundingClientRect = rect(30);
    act(() => {
      b.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0, clientY: 45 }));
      window.dispatchEvent(new MouseEvent("pointermove", { clientY: 42 })); // below the threshold: still a click
    });
    expect(calls.some((c) => c.op === "reorder_projects")).toBe(false);
    act(() => window.dispatchEvent(new MouseEvent("pointermove", { clientY: 5 })));
    const projects = within(nav).getByRole("list", { name: "Projects" });
    expect(within(projects).getAllByRole("button", { name: /^(a|b)$/ }).map((x) => x.textContent)).toEqual(["b", "a"]);
    expect(calls.some((c) => c.op === "reorder_projects")).toBe(false);
    act(() => window.dispatchEvent(new MouseEvent("pointerup", {})));
    // The click that ends the drag does not open the project.
    fireEvent.click(b);
    expect(window.location.hash).toBe("#/session/s-1");
    await waitFor(() => expect(calls.filter((c) => c.op === "reorder_projects").map((c) => c.input)).toEqual([{ paths: ["/p/b", "/p/a"] }]));
  });

  it("adds a project picked in the system dialog and opens it", async () => {
    const { calls } = renderWithDaemon(<App />, {
      ...base,
      choose_folder: () => ({ path: "/Users/me/proj" }),
      open_project: (i) => ({ root: i.path, name: "proj", git: true, sessions: [] }),
      get_preferences: () => ({ view: "simple", purposes: ["code"], setup: {}, documents: [] }),
    });
    await userEvent.click(await screen.findByRole("tab", { name: "Code" }));
    await userEvent.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByRole("heading", { name: "Build something" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Choose…" }));
    await waitFor(() => expect(calls.find((c) => c.op === "open_project")?.input).toEqual({ path: "/Users/me/proj" }));
    await waitFor(() => expect(window.location.hash).toBe("#/project/%2FUsers%2Fme%2Fproj"));
    expect(await screen.findByRole("heading", { name: "proj" })).toBeInTheDocument();
  });
});

describe("AddModel", () => {
  it("detects the model while typing and blocks what does not fit", async () => {
    renderWithDaemon(<AddModel />, {
      plan_model: (i) => ({
        address: {},
        repo: { id: String(i.address), gated: false },
        download_bytes: 18e9,
        existing: null,
        plan: { files: [{ path: "m.gguf", size: 18e9 }], size_bytes: 18e9, quant: "Q4_K_M", fit: String(i.address).includes("huge") ? "does_not_fit" : "fits", reason: "needs 80 GB, 20 GB available" },
      }),
      ...base,
    });
    const input = screen.getByRole("textbox");
    await userEvent.type(input, "demo/ok");
    expect(await screen.findByText(/Hugging Face · demo\/ok · 18.0 GB · fits ✓/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();
    await userEvent.clear(input);
    await userEvent.type(input, "demo/huge");
    expect(await screen.findByText(/needs 80 GB/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
  });

  it("adds with the chosen context after the click", async () => {
    const { calls } = renderWithDaemon(<AddModel />, {
      plan_model: () => ({ address: {}, repo: { id: "demo/m", gated: false }, download_bytes: 1, plan: { files: [], size_bytes: 1, fit: "fits", reason: "" } }),
      add_model: () => model("m"),
      ...base,
    });
    await userEvent.click(screen.getByRole("radio", { name: "large" }));
    await userEvent.type(screen.getByRole("textbox"), "demo/m");
    await screen.findByText(/fits ✓/);
    await userEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(calls.some((c) => c.op === "add_model")).toBe(true));
    const add = calls.find((c) => c.op === "add_model")!;
    expect(add.input).toMatchObject({ address: "demo/m", context: "large", start: true });
    expect(add.confirmed).toBe(true);
  });
});

describe("Connect", () => {
  it("connects with one click and explains failures", async () => {
    let connected = false;
    renderWithDaemon(<Connect />, {
      ...base,
      connections: () => [
        { client: "claude_code", connected, verified: connected, since: null },
        { client: "codex", connected: false, verified: false, since: null },
      ],
      connect_claude_code: () => {
        connected = true;
        return { client: "claude_code", connected: true, verified: true, actions: [], notes: "" };
      },
      connect_codex: () => {
        throw new Error("error: codex not found");
      },
    });
    // A line each, like the models: a dot, the name, its state, one button.
    const line = await screen.findByTestId("connection-claude_code");
    expect(line).toHaveTextContent("not connected");
    expect(line.querySelector(".dot-idle")).not.toBeNull();
    await userEvent.click(within(line).getByRole("button", { name: "Connect Claude Code" }));
    expect(await within(line).findByRole("button", { name: "Disconnect Claude Code" })).toHaveTextContent("Disconnect");
    expect(within(line).getByText("connected", { exact: true })).toBeInTheDocument();
    expect(line.querySelector(".dot-running")).not.toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Connect Codex" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Connecting Codex failed: error: codex not found");
  });
});

describe("App", () => {
  beforeEach(() => at("#/system"));
  afterEach(() => at("#/"));

  it("shows no model areas with one model and adds them with a second", async () => {
    let models = [model("chat", { roles: ["default"], status: "running" }), model("embed", { embedding: true, roles: ["embed"] })];
    const view = renderWithDaemon(<App />, { ...base, list_models: () => models });
    expect(await screen.findByText("chat")).toBeInTheDocument();
    expect(screen.queryByTestId("section-models")).toBeNull();
    expect(screen.queryByTestId("section-compare")).toBeNull();
    models = [...models, model("coder")];
    view.unmount();
    renderWithDaemon(<App />, { ...base, list_models: () => models });
    expect(await screen.findByTestId("section-models")).toBeInTheDocument();
    expect(screen.getByTestId("section-compare")).toBeInTheDocument();
  });
});

// covers: M7-AC-14
describe("Views", () => {
  afterEach(() => at("#/"));

  it("switch between simple and expert; the simple system page leaves out expert areas", async () => {
    at("#/system");
    let view = "simple";
    const models = [model("chat", { roles: ["default"], status: "running" }), model("coder")];
    const { calls } = renderWithDaemon(<App />, {
      ...base,
      list_models: () => models,
      get_preferences: () => ({ view, purposes: ["chat"], setup: {}, documents: [] }),
      set_preferences: (i) => {
        view = String(i.view);
        return { view, purposes: ["chat"], setup: {}, documents: [] };
      },
    });
    expect(await screen.findByRole("heading", { name: "System" })).toBeInTheDocument();
    expect(screen.queryByTestId("section-models")).toBeNull();
    expect(screen.queryByTestId("section-compare")).toBeNull();
    expect(screen.queryByRole("radio", { name: "read" })).toBeNull();
    const toggle = screen.getByRole("switch", { name: "Expert view" });
    expect(toggle).not.toBeChecked();
    await userEvent.click(toggle);
    await waitFor(() => expect(calls.find((c) => c.op === "set_preferences")?.input).toEqual({ view: "pro" }));
    expect(await screen.findByTestId("section-models")).toBeInTheDocument();
    expect(screen.getByTestId("section-compare")).toBeInTheDocument();
  });
});

describe("Updates", () => {
  it("is only shown in the desktop app, off by default, and saves the choice", async () => {
    const { Updates } = await import("./Settings");
    const settings = { auto_check: false };
    const first = renderWithDaemon(<Updates />, { get_update_settings: () => settings, set_update_settings: (i) => Object.assign(settings, i) });
    expect(first.container).toBeEmptyDOMElement();
    first.unmount();
    window.__ANCILO__ = { token: "t", app: true };
    try {
      const { calls } = renderWithDaemon(<Updates />, { get_update_settings: () => settings, set_update_settings: (i) => Object.assign(settings, i) });
      const box = await screen.findByRole("checkbox", { name: /Look for updates automatically/ });
      expect(box).not.toBeChecked();
      await userEvent.click(box);
      await waitFor(() => expect(calls.find((c) => c.op === "set_update_settings")?.input).toEqual({ auto_check: true }));
    } finally {
      delete window.__ANCILO__;
    }
  });
});
