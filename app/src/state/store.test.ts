import { nextActivity, nextProgress, nextTurn, queriesFor, type LiveTurn } from "./store";

describe("events refresh the right views", () => {
  it.each([
    ["instance.ready", "list_models"],
    ["download.progress", "list_models"],
    ["role.assigned", "list_models"],
    ["task.finished", "list_tasks"],
    ["compare.finished", "list_comparisons"],
    ["compare.run_finished", "comparison_report"],
    ["ab.guardrail_stopped", "ab_status"],
    ["settings.changed", "get_permissions"],
    ["route.set", "list_routes"],
    ["session.turn_finished", "get_session"],
    ["session.approval_required", "get_session"],
    ["session.applied", "list_sessions"],
    ["agent.file_changed", "session_diff"],
    ["terminal.opened", "list_terminals"],
    ["conversation.updated", "get_conversation"],
    ["conversation.updated", "list_conversations"],
    ["project.opened", "list_projects"],
    ["session.created", "list_projects"],
  ])("%s → %s", (kind, query) => {
    expect(queriesFor(kind).map((q) => q[0])).toContain(query);
  });

  it("ignores unrelated events", () => {
    expect(queriesFor("agent.step")).toEqual([]);
  });
});

describe("activity shown while Ancilo works", () => {
  const ev = (kind: string, data: Record<string, unknown> = {}) => ({ seq: 1, ts: "", kind, subject: "c-1", data });
  it("shows only the current turn – not the start of the previous answer", () => {
    let a = nextActivity({}, ev("agent.message", { text: "Ein Wald ist ein Ökosystem." }));
    expect(a["c-1"]).toEqual(["Ein Wald ist ein Ökosystem."]);
    a = nextActivity(a, ev("assistant.thinking", { model: "m" }));
    expect(a["c-1"]).toEqual([]);
    a = nextActivity(a, ev("agent.tool_called", { name: "list_models", arguments: {} }));
    expect(a["c-1"]).toEqual(["list_models {}"]);
    expect(nextActivity(a, ev("session.turn_started"))["c-1"]).toEqual([]);
    // Other subjects are untouched.
    expect(nextActivity({ x: ["keep"] }, ev("assistant.thinking"))["x"]).toEqual(["keep"]);
  });
});

describe("what the agent says on the way", () => {
  const ev = (kind: string, data: Record<string, unknown> = {}) => ({ seq: 1, ts: "", kind, subject: "s-1", data });
  it("is shown at once, with the steps so far – and starts over with the next turn", () => {
    let t: Record<string, LiveTurn> = {};
    const step = (kind: string, data: Record<string, unknown> = {}) => {
      t = nextTurn(t, ev(kind, data)) ?? t;
    };
    step("session.turn_started");
    step("agent.note", { text: "Ich prüfe die PDFs." });
    step("agent.tool_called", { name: "read_document" });
    step("agent.tool_called", { name: "read_document" });
    step("agent.note", { text: "  " });
    expect(t["s-1"]).toEqual({ notes: ["Ich prüfe die PDFs."], steps: 2 });
    expect(nextTurn(t, ev("agent.step"))).toBeNull();
    step("session.turn_started");
    expect(t["s-1"]).toEqual({ notes: [], steps: 0 });
  });
});

describe("what a chat does right now", () => {
  const ev = (kind: string, subject: string, data: Record<string, unknown> = {}) => ({ seq: 1, ts: "", kind, subject, data });
  it("says when a model loads, the web is searched and the answer is written", () => {
    let l = { loading: {}, phase: {} } as Parameters<typeof nextProgress>[0];
    const step = (kind: string, subject: string, data: Record<string, unknown> = {}) => {
      l = nextProgress(l, ev(kind, subject, data)) ?? l;
    };
    step("instance.starting", "qwen");
    expect(l.loading).toEqual({ qwen: true });
    step("instance.ready", "qwen");
    expect(l.loading).toEqual({});
    step("assistant.web_search", "c-1", { query: "forests in Europe" });
    expect(l.phase["c-1"]).toEqual({ step: "web", query: "forests in Europe" });
    step("assistant.answering", "c-1");
    expect(l.phase["c-1"]).toEqual({ step: "answer" });
    step("assistant.answer", "c-1");
    expect(l.phase["c-1"]).toBeUndefined();
  });
});
