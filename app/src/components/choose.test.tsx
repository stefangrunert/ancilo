import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { model, renderWithDaemon } from "../test-utils";
import { ModelChoice } from "./ModelChoice";

// Node's own (incomplete) localStorage shadows jsdom's: a small stand-in.
const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
});

function suggestion(id: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    name: id.toUpperCase(),
    address: `hf.co/org/${id}-GGUF:Q4_K_M`,
    maker: "Org",
    summary: { de: `${id} auf Deutsch.`, en: `${id} in English.` },
    license: "apache-2.0",
    released: "2026-04",
    tested: false,
    purposes: ["chat"],
    quality: 8,
    quant: "Q4_K_M",
    size_bytes: 4_500_000_000,
    download_bytes: 4_500_000_000,
    download_minutes: 12,
    ram_bytes: 6 * 2 ** 30,
    speed: "fast",
    tokens_per_sec: 40,
    measured: false,
    room: "comfortable",
    installed: null,
    ...extra,
  };
}

function recs(extra: Record<string, unknown> = {}) {
  return {
    purposes: ["chat"],
    best: suggestion("good", { tested: true }),
    alternatives: [suggestion("small", { quality: 4, speed: "ok" }), suggestion("big", { room: "close_programs" })],
    more: [suggestion("tiny", { quality: 2 })],
    too_big: 3,
    embedding: null,
    chip: "Apple M4 Pro",
    memory: { total_bytes: 24 * 2 ** 30, for_models_bytes: 16 * 2 ** 30, available_now_bytes: 12 * 2 ** 30, room_now_bytes: 11 * 2 ** 30, used_by_ancilo_bytes: 0 },
    catalog: { updated: "2026-10-01", source: "online", error: null },
    ...extra,
  };
}

// covers: M7-AC-12
describe("Choosing a model", () => {
  beforeEach(() => store.clear());

  it("downloads and starts with one click and shows the progress in plain words", async () => {
    const status = "downloading";
    const { calls } = renderWithDaemon(<ModelChoice purposes={["chat"]} />, {
      recommend_models: () => recs(),
      add_model: () => model("good-q4_k_m", { status }),
      list_models: () => [model("good-q4_k_m", { name: "GOOD", status, download: { bytes: 1, total: 4, percent: 25, bytes_per_sec: 1 } })],
      hardware_info: () => ({}),
      connections: () => [],
    });
    await userEvent.click(await screen.findByRole("button", { name: "Download and start" }));
    await waitFor(() => expect(calls.find((c) => c.op === "add_model")).toMatchObject({ input: { address: "hf.co/org/good-GGUF:Q4_K_M", start: true }, confirmed: true }));
    expect(await screen.findByText("Downloading GOOD …")).toBeInTheDocument();
    expect(screen.getByText(/25 % · about 1 min left · you can close the window meanwhile/)).toBeInTheDocument();
  });

  it("documents bring the search model along", async () => {
    const { calls } = renderWithDaemon(<ModelChoice purposes={["documents"]} />, {
      recommend_models: () => recs({ purposes: ["documents"], embedding: suggestion("embed", { address: "hf.co/org/embed-GGUF:Q8_0", download_bytes: 25_000_000 }) }),
      add_model: (i) => model(String(i.address).includes("embed") ? "embed-q8_0" : "good-q4_k_m"),
      list_models: () => [],
      hardware_info: () => ({}),
    });
    expect(await screen.findByText(/Ancilo also loads a small search model \(25 MB\)/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Download and start" }));
    await waitFor(() => expect(calls.filter((c) => c.op === "add_model").map((c) => c.input.address)).toEqual(["hf.co/org/embed-GGUF:Q8_0", "hf.co/org/good-GGUF:Q4_K_M"]));
  });

  it("uses an installed model without downloading", async () => {
    const { calls } = renderWithDaemon(<ModelChoice purposes={["chat"]} />, {
      recommend_models: () => recs({ best: suggestion("good", { installed: "good-q8_0", download_bytes: 0, download_minutes: 0 }) }),
      start_model: () => ({}),
      assign_role: () => ({}),
      list_models: () => [model("good-q8_0", { status: "running" })],
      connections: () => [],
    });
    const best = await screen.findByTestId("suggestion-good");
    expect(best).toHaveTextContent("already on your computer");
    await userEvent.click(within(best).getByRole("button", { name: "Use this one" }));
    await waitFor(() => expect(calls.find((c) => c.op === "start_model")?.input).toEqual({ model: "good-q8_0" }));
    expect(calls.find((c) => c.op === "assign_role")?.input).toEqual({ role: "default", model: "good-q8_0" });
    expect(await screen.findByText("Ready! Ancilo is set up.")).toBeInTheDocument();
  });

  it("says honestly when nothing fits, and searches Hugging Face on request", async () => {
    const { calls } = renderWithDaemon(<ModelChoice purposes={["chat"]} />, {
      // The search is part of the expert view.
      get_preferences: () => ({ view: "pro", purposes: ["chat"], setup: {}, documents: [] }),
      recommend_models: () => recs({ best: null, alternatives: [], more: [], memory: { ...recs().memory, total_bytes: 2 ** 30 } }),
      search_models: () => [{ repo: "someone/Fit-GGUF", address: "hf.co/someone/Fit-GGUF", downloads: 1234, likes: 1, updated: null, pipeline_tag: null }],
      plan_model: () => ({ address: {}, download_bytes: 1e9, plan: { fit: "fits" } }),
    });
    expect(await screen.findByText(/none of Ancilo's models fits comfortably on this computer \(1.0 GB memory\)/)).toBeInTheDocument();
    await userEvent.click(screen.getByText("Search Hugging Face yourself"));
    await userEvent.type(screen.getByRole("textbox", { name: "Search for a model" }), "fit{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "search_models")?.input).toEqual({ query: "fit", limit: 10 }));
    await userEvent.click(await screen.findByRole("button", { name: "Does it fit?" }));
    expect(await screen.findByText(/1.0 GB · fits ✓/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take this one" })).toBeInTheDocument();
  });
});
