import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ApiError } from "../api/client";
import { renderWithDaemon } from "../test-utils";
import { Cockpit } from "./Cockpit";
import { ErrorNote } from "./ui";

const GIB = 2 ** 30;

function preset(level: string) {
  const p: Record<string, Record<string, unknown>> = {
    eco: { keep_loaded_secs: 300, max_share: 0.35, parallel: 1, priority: "background", variant: "small", guard: "unload" },
    balanced: { keep_loaded_secs: 900, max_share: 0.55, parallel: 2, priority: "low", variant: "auto", guard: "unload_when_tight" },
    performance: { keep_loaded_secs: 3600, max_share: 0.7, parallel: 3, priority: "normal", variant: "precise", guard: "warn" },
    max: { keep_loaded_secs: null, max_share: 0.9, parallel: 4, priority: "normal", variant: "precise", guard: "warn" },
  };
  return { level, custom: false, ...p[level] };
}

function status(extra: Record<string, unknown> = {}) {
  return {
    settings: preset("balanced"),
    presets: ["eco", "balanced", "performance", "max"].map(preset),
    system: { available_bytes: 6 * GIB, pressure: "normal", thermal: "nominal", swap_used_bytes: 3 * GIB },
    total_bytes: 16 * GIB,
    cap_bytes: 8.8 * GIB,
    used_bytes: 3.6 * GIB,
    loaded: [{ id: "qwen3.5-4b-q4_k_m", name: "Qwen3.5 4B", ram_bytes: 3.6 * GIB, busy: false, pinned: false, idle_secs: 120, unload_in_secs: 780 }],
    recent: [{ at: "2026-10-01T12:00:00Z", model: "gemma", reason: "memory" }],
    ...extra,
  };
}

// covers: M7-AC-13
describe("Cockpit", () => {
  it("shows the level, its values and the computer right now – in plain words", async () => {
    renderWithDaemon(<Cockpit />, { get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }), resource_status: () => status() });
    const c = await screen.findByTestId("cockpit");
    expect(within(c).getByRole("slider", { name: "How much of your computer may Ancilo take?" })).toHaveAttribute("aria-valuetext", "Balanced");
    expect(c).toHaveTextContent("gives the memory back after a 15-minute break");
    expect(c).toHaveTextContent("Model stays loaded15 min");
    expect(c).toHaveTextContent("Memory at most55 %");
    expect(c).toHaveTextContent("Prioritylow");
    expect(within(c).getByRole("img", { name: "Ancilo uses 3.6 GB of 16 GB memory" })).toBeInTheDocument();
    expect(c).toHaveTextContent("Memory: relaxed");
    expect(c).toHaveTextContent("Temperature: normal");
    expect(c).toHaveTextContent("3.0 GB moved to disk");
    expect(c).toHaveTextContent("Qwen3.5 4B");
    expect(c).toHaveTextContent("unloaded in 13 min without use");
    expect(c).toHaveTextContent("gemma unloaded – memory got tight");
  });

  it("moves between levels and sets single values", async () => {
    let s = status();
    const { calls } = renderWithDaemon(<Cockpit />, {
      get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }),
      resource_status: () => s,
      set_resources: (i) => {
        s = status({ settings: { ...preset(String(i.level ?? "balanced")), ...i, custom: !i.level } });
        return s.settings;
      },
      unload_models: () => ({ models: ["qwen3.5-4b-q4_k_m"] }),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Eco" }));
    await waitFor(() => expect(calls.find((c) => c.op === "set_resources")?.input).toEqual({ level: "eco" }));
    expect(await screen.findByText(/Ancilo holds back/)).toBeInTheDocument();
    await userEvent.click(screen.getByText("Fine-tune"));
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Model stays loaded" }), "always");
    await waitFor(() => expect(calls.filter((c) => c.op === "set_resources").at(-1)?.input).toEqual({ keep_loaded_always: true }));
    expect(await screen.findByText("Custom settings")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Unload all now" }));
    await waitFor(() => expect(calls.some((c) => c.op === "unload_models")).toBe(true));
  });

  it("says plainly when memory is short or the computer hot", async () => {
    renderWithDaemon(<Cockpit />, { resource_status: () => status({ system: { available_bytes: GIB, pressure: "critical", thermal: "heavy", swap_used_bytes: 0 }, loaded: [] }) });
    const c = await screen.findByTestId("cockpit");
    expect(within(c).getByText("Memory: very tight")).toHaveClass("bad");
    expect(within(c).getByText("Temperature: hot")).toHaveClass("bad");
    expect(c).toHaveTextContent("No model is loaded – Ancilo takes no memory.");
  });

  it("follows the handle while it is dragged and saves the level once it rests", async () => {
    let s = status();
    const { calls } = renderWithDaemon(<Cockpit />, {
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      resource_status: () => s,
      set_resources: (i) => {
        s = status({ settings: preset(String(i.level)) });
        return s.settings;
      },
    });
    const slider = await screen.findByRole("slider");
    // Grabbed and moved: the handle follows the pointer between the levels …
    fireEvent.pointerDown(slider);
    fireEvent.change(slider, { target: { value: "1.6" } });
    expect(slider).toHaveValue("1.6");
    expect(slider).toHaveAttribute("aria-valuetext", "Performance");
    fireEvent.change(slider, { target: { value: "2.7" } });
    expect(slider).toHaveAttribute("aria-valuetext", "Maximum");
    expect(screen.getByText(/Everything for speed/)).toBeInTheDocument();
    expect(calls.some((c) => c.op === "set_resources")).toBe(false);
    // … and let go, it settles on the nearest level, saved once.
    fireEvent.pointerUp(slider);
    // (It glides there over a few frames – slow on a busy machine, hence the time.)
    await waitFor(() => expect(slider).toHaveValue("3"), { timeout: 5000 });
    await waitFor(() => expect(calls.filter((c) => c.op === "set_resources").map((c) => c.input)).toEqual([{ level: "max" }]));
    // Keys move level by level.
    fireEvent.keyDown(slider, { key: "ArrowLeft" });
    await waitFor(() => expect(calls.filter((c) => c.op === "set_resources").at(-1)?.input).toEqual({ level: "performance" }), { timeout: 5000 });
  });

  it("keeps it simple in the simple view: the slider and the computer now, no fine-tuning", async () => {
    renderWithDaemon(<Cockpit />, { get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }), resource_status: () => status() });
    const c = await screen.findByTestId("cockpit");
    expect(within(c).getByRole("slider")).toBeInTheDocument();
    expect(c).toHaveTextContent("Memory: relaxed");
    expect(within(c).queryByText("Fine-tune")).toBeNull();
    expect(c).not.toHaveTextContent("Requests at once");
  });
});

describe("Errors", () => {
  it("explain a lack of memory in plain words", () => {
    renderWithDaemon(<ErrorNote error={new ApiError("insufficient_resources", "'a': only 1.0 GB free", 507)} />, {});
    expect(screen.getByRole("alert")).toHaveTextContent("Your computer doesn't have enough free memory for this model right now.");
    expect(screen.getByRole("alert")).toHaveTextContent("only 1.0 GB free");
  });
});
