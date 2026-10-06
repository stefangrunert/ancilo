import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { renderWithDaemon } from "../test-utils";
import { ResultPreview } from "./ResultPreview";
import { splitGiven, TasksAreaPage, TaskView } from "./TasksArea";

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
    // No access mode: a task works on its own – the folder changes only when kept.
    expect(screen.queryByRole("combobox", { name: "Access" })).toBeNull();
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

  it("with files only: shows the result, saves it to Documents in one click, then opens or shows it", async () => {
    const where = { dir: "/Users/me/Documents", files: ["/Users/me/Documents/Übersicht.xlsx"], at: "" };
    let s: Record<string, unknown> = task({ free: true, project: "/home/.ancilo/tasks/x", changes: [{ path: "Übersicht.xlsx", added: 0, removed: 0, runs: false, change: "added" }] });
    const { calls } = renderWithDaemon(<TaskView id="s-1" />, {
      get_session: () => s,
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      save_results: () => {
        s = task({ free: true, changes: [], saved: where });
        return where;
      },
      open_document: () => ({ opened: true }),
      show_in_finder: () => ({ opened: true }),
      list_sessions: () => [],
    });
    const card = await screen.findByTestId("task-results");
    expect(card).toHaveTextContent("Result: 1 file(s)Übersicht.xlsx");
    // No folder of the task anywhere, nothing to keep.
    expect(screen.queryByTitle("/home/.ancilo/tasks/x")).toBeNull();
    expect(within(card).queryByRole("button", { name: "Keep" })).toBeNull();
    await userEvent.click(within(card).getByRole("button", { name: "Save to Documents" }));
    await waitFor(() => expect(calls.find((c) => c.op === "save_results")?.input).toEqual({ session: "s-1" }));
    const saved = await screen.findByTestId("task-saved");
    expect(saved).toHaveTextContent("Saved 1 file(s) in “Documents”.");
    await userEvent.click(within(saved).getByRole("button", { name: "Open" }));
    await waitFor(() => expect(calls.find((c) => c.op === "open_document")?.input).toEqual({ path: "/Users/me/Documents/Übersicht.xlsx" }));
    await userEvent.click(within(saved).getByRole("button", { name: "Show in Finder" }));
    await waitFor(() => expect(calls.find((c) => c.op === "show_in_finder")?.input).toEqual({ path: "/Users/me/Documents/Übersicht.xlsx" }));
  });

  // covers: FPL-03
  it("checks the results, shows one with the wrong cell marked, and saves only what was checked – after asking", async () => {
    const where = { dir: "/Users/me/Documents", files: ["/Users/me/Documents/Kosten.xlsx"], at: "" };
    let s: Record<string, unknown> = task({ free: true, changes_version: "v1", changes: [{ path: "Kosten.xlsx", added: 0, removed: 0, runs: false, change: "added" }] });
    const findings = [
      { area: "readable", level: "ok", message: "the file opens and can be read" },
      { area: "numbers", level: "error", message: "the total of “Betrag” says 990, but the rows above add up to 980", place: "2025!B4" },
    ];
    const { calls } = renderWithDaemon(<TaskView id="s-1" />, {
      get_session: () => s,
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      check_results: () => ({ version: "v1", files: [{ path: "Kosten.xlsx", file: "ab12cd34ef56ab78", worst: "error", errors: 1, warnings: 0 }] }),
      preview_result: () => ({
        path: "Kosten.xlsx",
        version: "v1",
        file: "ab12cd34ef56ab78",
        findings,
        layout: {
          kind: "spreadsheet",
          sheets: [{ name: "2025", total_rows: 4, rows: [{ number: 1, cells: ["Posten", "Betrag"] }, { number: 2, cells: ["Miete", "900"] }, { number: 3, cells: ["Strom", "80"] }, { number: 4, cells: ["Summe", "990"] }] }],
          limits: [{ kind: "no_formatting" }],
        },
      }),
      save_results: () => {
        s = task({ free: true, changes: [], saved: where });
        return where;
      },
      list_sessions: () => [],
    });
    const card = await screen.findByTestId("task-results");
    expect(await within(card).findByTestId("check-badge")).toHaveTextContent("1 problem(s)");
    await userEvent.click(within(card).getByRole("button", { name: "Look at it" }));
    const view = await screen.findByTestId("result-preview");
    await waitFor(() => expect(within(view).getByTestId("marked-cell")).toHaveTextContent("990"));
    expect(within(view).getByTestId("finding-error")).toHaveTextContent("rows above add up to 980");
    expect(within(view).getByTestId("preview-limits")).toHaveTextContent("Fonts, colours, column widths");
    expect(view).toHaveTextContent("cannot tell whether the content is right");
    await userEvent.click(within(view).getByRole("button", { name: "Close" }));
    // Saving what the check found wrong: asked first; then exactly the checked version.
    await userEvent.click(within(card).getByRole("button", { name: "Save to Documents" }));
    expect(calls.find((c) => c.op === "save_results")).toBeUndefined();
    await userEvent.click(await screen.findByTestId("anyway"));
    await waitFor(() => expect(calls.find((c) => c.op === "save_results")?.input).toEqual({ session: "s-1", version: "v1" }));
  });

  it("a preview of an earlier version says so and offers the new one – never swaps it", async () => {
    let version = "v1";
    function Harness() {
      const [current, setCurrent] = useState("v1");
      return (
        <>
          <button type="button" onClick={() => setCurrent("v2")}>
            task changed
          </button>
          <ResultPreview session="s-1" path="Brief.docx" current={current} onClose={() => {}} />
        </>
      );
    }
    renderWithDaemon(<Harness />, {
      preview_result: () => ({ path: "Brief.docx", version, file: "aa", findings: [], layout: { kind: "word", blocks: [{ kind: "heading", level: 1, text: version === "v1" ? "Alt" : "Neu" }] } }),
    });
    const view = await screen.findByTestId("result-preview");
    await within(view).findByText("Alt");
    expect(within(view).queryByTestId("preview-stale")).toBeNull();
    // The task changed its results: still the old text, with the offer.
    version = "v2";
    await userEvent.click(screen.getByRole("button", { name: "task changed", hidden: true }));
    expect(await within(view).findByTestId("preview-stale")).toHaveTextContent("You still see the earlier version");
    expect(within(view).getByText("Alt")).toBeInTheDocument();
    await userEvent.click(within(view).getByRole("button", { name: "Show the new version" }));
    await within(view).findByText("Neu");
    expect(within(view).queryByTestId("preview-stale")).toBeNull();
  });
});

// covers: M10-AC-04
describe("A new task", () => {
  it("needs a folder first; the system's dialog chooses it, and an example fills the input", async () => {
    const { calls } = renderWithDaemon(<TasksAreaPage />, {
      choose_folder: () => ({ path: "/Users/me/Verträge" }),
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
    });
    const ask = await screen.findByRole("textbox", { name: "What should Ancilo do?" });
    expect(ask).toBeDisabled();
    expect(screen.getByText("Please choose a folder. You can change it later or add more folders.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Choose a folder" }));
    const chosen = await screen.findByTestId("task-folder-chosen");
    expect(chosen).toHaveTextContent("Verträge");
    expect(calls.find((c) => c.op === "choose_folder")?.input).toEqual({ prompt: "Choose the folder Ancilo should work in" });
    await userEvent.click(within(screen.getByRole("group", { name: "For example:" })).getByRole("button", { name: "Sort the files by year" }));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "What should Ancilo do?" })).toHaveValue("Sort the files by year"));
    expect(screen.getByRole("textbox", { name: "What should Ancilo do?" })).toBeEnabled();
  });

  it("on a folder's own page, the folder is fixed", async () => {
    renderWithDaemon(<TasksAreaPage folder="/Users/me/Belege" />, {
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
    });
    const chosen = await screen.findByTestId("task-folder-chosen");
    expect(chosen).toHaveTextContent("Belege");
    expect(within(chosen).queryByRole("button", { name: "Change" })).toBeNull();
    expect(screen.getByRole("textbox", { name: "What should Ancilo do?" })).toBeEnabled();
  });
});

describe("Files given with a message", () => {
  it("are shown as files, not as text", () => {
    expect(splitGiven("Fass zusammen\n\n(Files the user gave for this:\n- Belege/a, b.pdf\n- c.pdf)")).toEqual({
      text: "Fass zusammen",
      files: ["Belege/a, b.pdf", "c.pdf"],
    });
    expect(splitGiven("Nur Text (mit Klammer)")).toEqual({ text: "Nur Text (mit Klammer)", files: [] });
  });
});
