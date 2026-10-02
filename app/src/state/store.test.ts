import { nextActivity, queriesFor } from "./store";

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
