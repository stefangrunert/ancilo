import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { model, renderWithDaemon, type Handler } from "../test-utils";
import { SetupPage } from "./Setup";

function suggestion(id: string) {
  return {
    id,
    name: id.toUpperCase(),
    address: `hf.co/org/${id}-GGUF:Q4_K_M`,
    maker: "Org",
    summary: { de: "Gut.", en: "Good." },
    license: "apache-2.0",
    released: "2026-04",
    tested: true,
    purposes: ["chat"],
    quality: 8,
    quant: "Q4_K_M",
    size_bytes: 4.5e9,
    download_bytes: 4.5e9,
    download_minutes: 12,
    ram_bytes: 6 * 2 ** 30,
    speed: "fast",
    tokens_per_sec: 40,
    measured: false,
    room: "comfortable",
    installed: null,
  };
}

const GIB = 2 ** 30;

/** A daemon whose state follows what the setup does. */
function daemon() {
  const prefs: { view: string; purposes: string[]; setup: Record<string, string>; documents: string[] } = { view: "simple", purposes: [], setup: {}, documents: [] };
  let models: unknown[] = [];
  let level = "balanced";
  const projects: unknown[] = [];
  const ops: Record<string, Handler> = {
    get_preferences: () => prefs,
    set_preferences: (i) => {
      if (i.purposes) prefs.purposes = i.purposes as string[];
      if (i.step) {
        if (i.state === "open") delete prefs.setup[String(i.step)];
        else prefs.setup[String(i.step)] = String(i.state);
      }
      if (i.add_documents) prefs.documents.push(String(i.add_documents));
      return prefs;
    },
    list_models: () => models,
    recommend_models: () => ({
      purposes: prefs.purposes,
      best: suggestion("good"),
      alternatives: [],
      more: [],
      too_big: 0,
      embedding: null,
      chip: "Apple M4",
      memory: { total_bytes: 16 * GIB, for_models_bytes: 8 * GIB, available_now_bytes: 8 * GIB, room_now_bytes: 7 * GIB, used_by_ancilo_bytes: 0 },
      catalog: { updated: "2026-10-01", source: "bundled", error: null },
    }),
    add_model: () => {
      models = [model("good-q4_k_m", { name: "GOOD", status: "running" })];
      return models[0];
    },
    hardware_info: () => ({}),
    resource_status: () => ({
      settings: { level, custom: false, keep_loaded_secs: 900, max_share: 0.55, parallel: 2, priority: "low", variant: "auto", guard: "unload_when_tight" },
      presets: [],
      system: { available_bytes: 8 * GIB, pressure: "normal", thermal: "nominal", swap_used_bytes: 0 },
      total_bytes: 16 * GIB,
      cap_bytes: 8 * GIB,
      used_bytes: 0,
      loaded: [],
      recent: [],
    }),
    set_resources: (i) => {
      level = String(i.level);
      return {};
    },
    connections: () => [
      { client: "claude_code", connected: false, verified: false, since: null },
      { client: "codex", connected: false, verified: false, since: null },
    ],
    list_projects: () => projects,
    create_project: (i) => {
      const p = { root: `/Users/me/Ancilo/${String(i.name)}`, name: String(i.name), sessions: 0, last_used: "", exists: true, area: "code" };
      projects.push(p);
      return { ...p, git: true, sessions: [] };
    },
  };
  return { ops, prefs };
}

const next = () => userEvent.click(screen.getByRole("button", { name: "Next" }));

// covers: M7-AC-14
describe("Set up", () => {
  it("leads step by step through everything, explains each step, and ends in a checklist", async () => {
    const { ops, prefs } = daemon();
    const { calls } = renderWithDaemon(<SetupPage />, ops);
    // 1: what for – explained, with a progress indicator.
    const purpose = await screen.findByTestId("step-purpose");
    expect(purpose).toHaveTextContent("Step 1 of 5");
    expect(purpose).toHaveTextContent("Ancilo picks the AI that suits your purpose");
    await userEvent.click(within(purpose).getByRole("checkbox", { name: /Programming/ }));
    await next();
    await waitFor(() => expect(prefs.setup.purpose).toBe("done"));
    expect(prefs.purposes).toEqual(["chat", "code"]);
    // 2: the AI – recommended, downloaded with one click.
    const modelStep = await screen.findByTestId("step-model");
    expect(modelStep).toHaveTextContent("The AI runs entirely on your computer");
    await userEvent.click(await within(modelStep).findByRole("button", { name: "Download and start" }));
    expect(await screen.findByText("The AI is ready.")).toBeInTheDocument();
    await next();
    // 3: consideration for the computer.
    const res = await screen.findByTestId("step-resources");
    expect(res).toHaveTextContent("Step 3 of 5");
    await userEvent.click(within(res).getByRole("radio", { name: /Eco/ }));
    await waitFor(() => expect(calls.find((c) => c.op === "set_resources")?.input).toEqual({ level: "eco" }));
    await next();
    // 4: Claude Code / Codex – optional, skipped.
    const connect = await screen.findByTestId("step-connect");
    expect(connect).toHaveTextContent("Claude Code (Anthropic) and Codex (OpenAI) are programming assistants");
    expect(within(connect).getByRole("button", { name: "Next" })).toBeDisabled();
    await userEvent.click(within(connect).getByRole("button", { name: "Skip" }));
    // 5: projects – a name is enough.
    const projects = await screen.findByTestId("step-projects");
    await userEvent.type(within(projects).getByRole("textbox", { name: "New project – what should it be called?" }), "Recipes{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "create_project")?.input).toEqual({ name: "Recipes" }));
    await waitFor(() => expect(within(projects).getByRole("button", { name: "Next" })).toBeEnabled());
    await next();
    // Done: a checklist with a way to change each step.
    const list = await screen.findByTestId("setup-checklist");
    expect(list).toHaveTextContent("Ancilo is set up.");
    expect(list).toHaveTextContent("Chat and write, Programming");
    expect(list).toHaveTextContent("GOOD");
    expect(list).toHaveTextContent("Eco");
    expect(list).toHaveTextContent("skipped");
    expect(list).toHaveTextContent("1 project(s)");
    await userEvent.click(within(list).getByRole("button", { name: "Change: Consideration for your computer" }));
    expect(await screen.findByTestId("step-resources")).toBeInTheDocument();
    await next();
    expect(await screen.findByTestId("setup-checklist")).toBeInTheDocument();
  });

  it("goes back a step", async () => {
    const { ops, prefs } = daemon();
    prefs.purposes = ["chat"];
    prefs.setup = { purpose: "done" };
    renderWithDaemon(<SetupPage />, ops);
    await screen.findByTestId("step-model");
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByTestId("step-purpose")).toBeInTheDocument();
  });
});
