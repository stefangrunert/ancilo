import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithDaemon, type Handler } from "../test-utils";
import { ConversationView } from "./Conversation";
import { WebSearchPage } from "./WebSearch";

function settings(extra: Record<string, unknown> = {}) {
  return { provider: "off", mode: "ask", serper_key: null, ...extra };
}

// covers: M7-AC-16
describe("Web search page", () => {
  it("is off until chosen, says what goes out, and sets provider and mode", async () => {
    let s = settings();
    const { calls } = renderWithDaemon(<WebSearchPage />, {
      get_web_search: () => s,
      set_web_search: (i) => (s = settings({ ...s, ...i })),
      test_web_search: () => ({ provider: "wikipedia", sources: 2, took_ms: 800 }),
    });
    const page = await screen.findByTestId("web-search");
    expect(within(page).getByRole("radio", { name: /^Off/ })).toBeChecked();
    expect(page).toHaveTextContent("Only a short search query goes to the provider you choose");
    expect(page).toHaveTextContent("those websites see your internet address");
    expect(screen.queryByRole("radiogroup", { name: "When Ancilo searches" })).toBeNull();
    await userEvent.click(within(page).getByRole("radio", { name: /^Wikipedia/ }));
    await waitFor(() => expect(calls.find((c) => c.op === "set_web_search")?.input).toEqual({ provider: "wikipedia" }));
    const mode = await screen.findByRole("radiogroup", { name: "When Ancilo searches" });
    expect(within(mode).getByRole("radio", { name: /Ask me first/ })).toBeChecked();
    await userEvent.click(within(mode).getByRole("radio", { name: /Automatically/ }));
    await waitFor(() => expect(calls.filter((c) => c.op === "set_web_search").at(-1)?.input).toEqual({ mode: "auto" }));
    await userEvent.click(screen.getByRole("button", { name: "Try a search" }));
    expect(await screen.findByText("Works – 2 source(s) in 0.8 s.")).toBeInTheDocument();
  });

  it("guides through Serper and saves a key only after a test search worked", async () => {
    let s = settings();
    let good = false;
    const { calls } = renderWithDaemon(<WebSearchPage />, {
      get_web_search: () => s,
      set_web_search: (i) => (s = settings({ ...s, ...i, serper_key: i.serper_key ? "…-key" : s.serper_key })),
      test_web_search: () => {
        if (!good) throw Object.assign(new Error("Serper does not accept this key – copy it again from serper.dev (API key)"), { code: "unauthorized" });
        return { provider: "serper", sources: 1, took_ms: 600 };
      },
    });
    await userEvent.click(await screen.findByRole("radio", { name: /^Google/ }));
    const setup = await screen.findByTestId("serper-setup");
    expect(within(setup).getByRole("link", { name: "serper.dev" })).toHaveAttribute("href", "https://serper.dev/");
    expect(calls.some((c) => c.op === "set_web_search"), "nothing set without a key").toBe(false);
    const key = within(setup).getByLabelText("Serper API key");
    expect(key).toHaveAttribute("type", "password");
    await userEvent.type(key, "wrong-key");
    await userEvent.click(within(setup).getByRole("button", { name: "Check and save" }));
    expect(await within(setup).findByRole("alert")).toHaveTextContent("does not accept this key");
    expect(calls.some((c) => c.op === "set_web_search")).toBe(false);
    good = true;
    await userEvent.clear(key);
    await userEvent.type(key, "test-key{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "set_web_search")?.input).toEqual({ serper_key: "test-key", provider: "serper" }));
    expect(calls.filter((c) => c.op === "test_web_search").at(-1)?.input).toEqual({ provider: "serper", serper_key: "test-key" });
    expect(await screen.findByTestId("serper-key")).toHaveTextContent("Your Serper key is saved (…-key).");
  });
});

function chat(messages: Record<string, unknown>[], more: Record<string, Handler> = {}, web = settings({ provider: "wikipedia" })) {
  return renderWithDaemon(<ConversationView id="c-1" />, {
    get_conversation: () => ({ id: "c-1", title: "Oslo", kind: "chat", created_at: "", updated_at: "", messages }),
    pending_actions: () => [],
    get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
    get_web_search: () => web,
    list_conversations: () => [],
    ...more,
  });
}

const question = { role: "user", text: "Wie viele Einwohner hat Oslo?", at: "" };

// covers: M6-AC-14
describe("Web search in a chat", () => {
  it("asks first – with the query to change – and sends exactly the decision", async () => {
    const proposal = { role: "assistant", text: "", at: "", web: { state: "proposed", query: "Einwohnerzahl Oslo", provider: "wikipedia" } };
    const { calls } = chat([question, proposal], { answer_web_proposal: () => ({}) });
    const card = await screen.findByTestId("web-proposal");
    expect(card).toHaveTextContent("Shall I look this up on the web?");
    expect(card).toHaveTextContent("Only this search query goes to Wikipedia:");
    const q = within(card).getByRole("textbox", { name: "Search query" });
    expect(q).toHaveValue("Einwohnerzahl Oslo");
    await userEvent.clear(q);
    await userEvent.type(q, "Oslo Einwohner 2026");
    await userEvent.click(within(card).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(calls.find((c) => c.op === "answer_web_proposal")?.input).toEqual({ conversation: "c-1", search: true, query: "Oslo Einwohner 2026" }));
    await userEvent.click(within(card).getByRole("button", { name: "Answer without the web" }));
    await waitFor(() => expect(calls.filter((c) => c.op === "answer_web_proposal").at(-1)?.input).toEqual({ conversation: "c-1", search: false, query: null }));
  });

  it("shows what was searched and the sources as links", async () => {
    chat([
      question,
      { role: "assistant", text: "", at: "", web: { state: "accepted", query: "Oslo Einwohner", provider: "wikipedia" } },
      {
        role: "assistant",
        text: "Oslo hat 728.714 Einwohner [1].",
        at: "",
        web: { state: "searched", query: "Oslo Einwohner", provider: "wikipedia", sources: [{ n: 1, title: "Oslo", url: "https://de.wikipedia.org/wiki/Oslo" }] },
      },
    ]);
    const sources = await screen.findByTestId("web-sources");
    expect(sources).toHaveTextContent("Searched the web for “Oslo Einwohner” · Wikipedia");
    expect(within(sources).getByRole("link", { name: "Oslo" })).toHaveAttribute("href", "https://de.wikipedia.org/wiki/Oslo");
    expect(within(screen.getByTestId("assistant-answer")).getByRole("link", { name: "[1]" })).toHaveAttribute("href", "https://de.wikipedia.org/wiki/Oslo");
    // The decided proposal leaves no empty bubble.
    expect(screen.getAllByTestId("assistant-answer")).toHaveLength(1);
    expect(screen.queryByTestId("web-proposal")).toBeNull();
  });

  it("offers to set it up while it is off – and has no switch then", async () => {
    const offer = { role: "assistant", text: "Etwa 700.000, das kann veraltet sein.", at: "", web: { state: "offer" } };
    chat([question, offer], {}, settings());
    const card = await screen.findByTestId("web-offer");
    expect(screen.queryByRole("switch", { name: "Web search" })).toBeNull();
    await userEvent.click(within(card).getByRole("button", { name: "Set up web search" }));
    expect(window.location.hash).toBe("#/web");
    window.history.replaceState(null, "", "#/");
  });

  it("has a web search switch: off asks first, on searches without asking", async () => {
    let web = settings({ provider: "wikipedia" });
    const { calls } = renderWithDaemon(<ConversationView id="c-1" />, {
      get_conversation: () => ({ id: "c-1", title: "Oslo", kind: "chat", created_at: "", updated_at: "", messages: [] }),
      pending_actions: () => [],
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: [] }),
      get_web_search: () => web,
      set_web_search: (i) => (web = settings({ ...web, ...i })),
      list_conversations: () => [],
      ask: () => ({ answer: "ok", conversation: "c-1" }),
    });
    const toggle = await screen.findByRole("switch", { name: "Web search" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await userEvent.click(toggle);
    await waitFor(() => expect(calls.find((c) => c.op === "set_web_search")?.input).toEqual({ mode: "auto" }));
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
    // A message goes as it is: the switch is the setting, not a per-message flag.
    await userEvent.type(screen.getByRole("textbox", { name: "What should Ancilo do?" }), "Wetter in Oslo?{Enter}");
    await waitFor(() => expect(calls.find((c) => c.op === "ask")?.input).toEqual({ prompt: "Wetter in Oslo?", conversation: "c-1" }));
    await userEvent.click(toggle);
    await waitFor(() => expect(calls.filter((c) => c.op === "set_web_search").pop()?.input).toEqual({ mode: "ask" }));
  });
});
