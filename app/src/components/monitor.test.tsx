import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithDaemon, type Handler } from "../test-utils";
import { MonitorAlerts, StatusBar } from "./SystemMonitor";

const GIB = 2 ** 30;

function health(extra: Record<string, unknown> = {}) {
  return {
    level: "ok",
    causes: [],
    memory_percent: 62,
    cpu_percent: 18,
    pressure: "normal",
    thermal: "nominal",
    ancilo_bytes: 0,
    mostly_others: false,
    consumers: [],
    fixes: [],
    ...extra,
  };
}

function resources(extra: Record<string, unknown> = {}) {
  const settings = { level: "balanced", custom: false, keep_loaded_secs: 900, max_share: 0.55, parallel: 2, priority: "low", variant: "auto", guard: "unload_when_tight" };
  return {
    settings,
    presets: [],
    system: { available_bytes: 6 * GIB, pressure: "normal", thermal: "nominal", swap_used_bytes: 0 },
    total_bytes: 16 * GIB,
    cap_bytes: 8 * GIB,
    used_bytes: 0,
    loaded: [],
    recent: [],
    ...extra,
  };
}

function monitor(ops: Record<string, Handler>) {
  return renderWithDaemon(
    <>
      <StatusBar />
      <MonitorAlerts />
    </>,
    { get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }), list_models: () => [], ...ops },
  );
}

// An in-memory store (Node's own `localStorage` is not usable without a file).
const store = new Map<string, string>();
beforeEach(() => {
  store.clear();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  });
});
afterEach(() => vi.unstubAllGlobals());

// covers: M7-AC-15
describe("System monitor", () => {
  it("shows how the computer is doing in one line – and no warning while all is calm", async () => {
    monitor({ system_health: () => health(), resource_status: () => resources() });
    const bar = await screen.findByTestId("statusbar");
    await waitFor(() => expect(within(bar).getByTestId("verdict")).toHaveTextContent("All calm"));
    expect(bar).toHaveTextContent("Memory 62 %");
    expect(bar).toHaveTextContent("Processor 18 %");
    expect(within(bar).getByRole("button", { name: /Ancilo is idle/ })).toBeInTheDocument();
    expect(within(bar).getByRole("button", { name: "Balanced" })).toBeInTheDocument();
    expect(screen.queryByTestId("monitor-warning")).toBeNull();
    // The app's settings sit at the right end of the bar.
    const settings = within(bar).getByRole("group", { name: "Settings" });
    expect(within(settings).getByRole("switch", { name: "Expert view" })).not.toBeChecked();
    expect(within(settings).getByRole("combobox", { name: "Language" })).toHaveValue("en");
  });

  it("warns when it gets tight, names other programs and helps with one click", async () => {
    const h = health({
      level: "tight",
      causes: ["memory"],
      memory_percent: 96,
      ancilo_bytes: 4 * GIB,
      mostly_others: true,
      consumers: [
        { name: "Google Chrome", memory_bytes: 8 * GIB, cpu_percent: 10, ancilo: false },
        { name: "Simulator", memory_bytes: 3 * GIB, cpu_percent: 2, ancilo: false },
      ],
      fixes: [{ kind: "activity_monitor" }, { kind: "unload_models", frees_bytes: 4 * GIB }, { kind: "level_eco" }],
    });
    const { calls } = monitor({
      system_health: () => h,
      resource_status: () => resources(),
      unload_models: () => ({ models: ["qwen"] }),
      set_resources: () => resources().settings,
      open_activity_monitor: () => ({ opened: true }),
    });
    const card = await screen.findByTestId("monitor-warning");
    expect(card).toHaveRole("alert");
    expect(within(card).getByRole("heading")).toHaveTextContent("Your computer is getting tight");
    expect(card).toHaveTextContent("The memory is almost full");
    expect(card).toHaveTextContent("Mostly other programs: Google Chrome (8.0 GB), Simulator (3.0 GB).");
    expect(screen.getByTestId("verdict")).toHaveTextContent("Computer is busy");
    await userEvent.click(within(card).getByRole("button", { name: "Open Activity Monitor" }));
    await waitFor(() => expect(calls.some((c) => c.op === "open_activity_monitor")).toBe(true));
    await userEvent.click(within(card).getByRole("button", { name: "Unload the AI (frees 4.0 GB)" }));
    await waitFor(() => expect(calls.some((c) => c.op === "unload_models")).toBe(true));
    await userEvent.click(within(card).getByRole("button", { name: "Switch to Eco" }));
    await waitFor(() => expect(calls.find((c) => c.op === "set_resources")?.input).toEqual({ level: "eco" }));
  });

  it("“Later” keeps it quiet – until it gets worse", async () => {
    let h = health({ level: "tight", causes: ["cpu"], mostly_others: false, ancilo_bytes: 2 * GIB, fixes: [{ kind: "level_eco" }] });
    const { queryClient } = monitor({ system_health: () => h, resource_status: () => resources() });
    const card = await screen.findByTestId("monitor-warning");
    expect(card).toHaveTextContent("Mostly Ancilo's AI (2.0 GB).");
    await userEvent.click(within(card).getByRole("button", { name: "Later" }));
    expect(screen.queryByTestId("monitor-warning")).toBeNull();
    // Still tight: quiet.
    await act(() => queryClient.refetchQueries({ queryKey: ["system_health"] }));
    expect(screen.queryByTestId("monitor-warning")).toBeNull();
    // Worse: it is back.
    h = health({ level: "critical", causes: ["heat"], fixes: [] });
    await act(() => queryClient.refetchQueries({ queryKey: ["system_health"] }));
    const again = await screen.findByTestId("monitor-warning");
    expect(again).toHaveTextContent("Your computer is at its limit");
    expect(again).toHaveTextContent("Closing programs you do not need helps.");
  });

  it("says what Ancilo did by itself in an emergency, with undo", async () => {
    const at = new Date(Date.now() - 60_000).toISOString();
    const { calls } = monitor({
      system_health: () => health(),
      resource_status: () =>
        resources({
          recent: [
            { at, model: "qwen", reason: "memory" },
            { at: new Date(Date.now() - 120_000).toISOString(), model: "old", reason: "idle" },
          ],
        }),
      list_models: () => [{ id: "qwen", name: "Qwen3.5 9B" }],
      start_model: () => ({}),
    });
    const card = await screen.findByTestId("monitor-acted");
    await waitFor(() => expect(card).toHaveTextContent("Memory was running out, so Ancilo unloaded Qwen3.5 9B."));
    await userEvent.click(within(card).getByRole("button", { name: "Load again now" }));
    await waitFor(() => expect(calls.find((c) => c.op === "start_model")?.input).toEqual({ model: "qwen" }));
    await waitFor(() => expect(screen.queryByTestId("monitor-acted")).toBeNull());
    // Not shown again.
    expect(Number(localStorage.getItem("ancilo.guardSeen"))).toBe(Date.parse(at));
  });
});
