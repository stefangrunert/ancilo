import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithDaemon } from "../test-utils";
import { TaskView } from "./TasksArea";

function task(extra: Record<string, unknown> = {}) {
  return {
    id: "s-1",
    title: "Rechnungen",
    project: "/Users/me/Belege",
    model: "chat",
    permission: "shell",
    status: "idle",
    isolated: true,
    workdir: "/w",
    turns: 1,
    changes: [],
    approvals: [],
    variants: [],
    can_retry: false,
    web_used: false,
    kind: "task",
    messages: [{ role: "user", text: "Mach eine Tabelle" }, { role: "assistant", text: "Fertig." }],
    created_at: "",
    ...extra,
  };
}

// covers: M10-AC-04
describe("A task", () => {
  it("shows what changed in the copy, keeps exactly what was shown, and drops only after asking", async () => {
    const s = task({
      changes_version: "v-1",
      changes: [
        { path: "Übersicht.xlsx", added: 0, removed: 0, runs: false, change: "added" },
        { path: "2025/Strom.pdf", added: 0, removed: 0, runs: false, change: "renamed", from: "Strom.pdf" },
      ],
    });
    const { calls } = renderWithDaemon(<TaskView id="s-1" />, {
      get_session: () => s,
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      apply_changes: () => ({ files: [] }),
      discard_changes: () => ({ files: [] }),
      list_sessions: () => [],
    });
    const card = await screen.findByTestId("task-changes");
    expect(card).toHaveTextContent("2 change(s)");
    expect(card).toHaveTextContent("nothing in “Belege” has changed yet");
    const lines = within(card).getAllByTestId("task-change");
    expect(lines[0]).toHaveTextContent("newÜbersicht.xlsx");
    expect(lines[1]).toHaveTextContent("moved2025/Strom.pdffrom Strom.pdf");
    await userEvent.click(within(card).getByRole("button", { name: "Drop" }));
    expect(calls.some((c) => c.op === "discard_changes")).toBe(false);
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Drop" }));
    await waitFor(() => expect(calls.find((c) => c.op === "discard_changes")?.input).toEqual({ session: "s-1", paths: null }));
    await userEvent.click(within(card).getByRole("button", { name: "Keep" }));
    await waitFor(() => expect(calls.find((c) => c.op === "apply_changes")).toMatchObject({ input: { session: "s-1", paths: null, version: "v-1" }, confirmed: true }));
    // The task's own access modes – no commands here.
    const mode = screen.getByRole("combobox", { name: "Access" });
    expect(mode.closest("label")?.getAttribute("title")).toContain("your folder gets only what you keep");
  });

  it("offers to undo what was kept – after asking", async () => {
    const s = task({ applied: { id: "ap-1", at: "", changes: [{ kind: "added", path: "a.xlsx", size: 1 }] } });
    const { calls } = renderWithDaemon(<TaskView id="s-1" />, {
      get_session: () => s,
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      undo_apply: () => ({}),
      list_sessions: () => [],
    });
    const done = await screen.findByTestId("task-applied");
    expect(done).toHaveTextContent("Kept: 1 change(s) are in “Belege” now.");
    await userEvent.click(within(done).getByRole("button", { name: "Undo" }));
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(calls.find((c) => c.op === "undo_apply")).toMatchObject({ input: { session: "s-1" }, confirmed: true }));
  });
});
