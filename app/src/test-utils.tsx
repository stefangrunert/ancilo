import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import type { ReactNode } from "react";
import { Client } from "./api/client";
import { I18nProvider } from "./i18n";
import { ClientProvider } from "./state/store";

export type Handler = (input: Record<string, unknown>, confirmed: boolean) => unknown;

/** Renders with a fake daemon: `ops` answers operations; calls are recorded. */
export function renderWithDaemon(ui: ReactNode, ops: Record<string, Handler>) {
  const calls: { op: string; input: Record<string, unknown>; confirmed: boolean }[] = [];
  const fetchImpl: typeof fetch = async (url, init) => {
    const u = String(url);
    if (u.includes("/api/v1/events")) {
      // An open stream that never sends anything.
      return new Response(new ReadableStream({ start() {} }), { status: 200 });
    }
    // A document sent to read: handled like an operation "upload" with its name.
    const upload = u.includes("/api/v1/attachments");
    const op = upload ? "upload" : (u.split("/api/v1/ops/")[1] ?? "");
    const input = upload ? { name: new URL(u, "http://x").searchParams.get("name") } : (JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>);
    const confirmed = (init?.headers as Record<string, string>)["x-ancilo-confirm"] === "true";
    calls.push({ op, input, confirmed });
    const h = ops[op];
    if (!h) return new Response(JSON.stringify({ error: { code: "not_found", message: `no op ${op}` } }), { status: 404 });
    try {
      return new Response(JSON.stringify(h(input, confirmed) ?? {}), { status: 200 });
    } catch (e) {
      const err = e as { code?: string; message: string };
      return new Response(JSON.stringify({ error: { code: err.code ?? "invalid_input", message: err.message } }), { status: 400 });
    }
  };
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const client = new Client("", "t", fetchImpl);
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ClientProvider client={client} queryClient={queryClient}>
        <I18nProvider initial="en">{ui}</I18nProvider>
      </ClientProvider>
    </QueryClientProvider>,
  );
  return { ...result, calls, queryClient };
}

export function model(id: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    name: id,
    quant: "Q8_0",
    size_bytes: 1000,
    source: "Hugging Face · demo",
    path: null,
    status: "ready",
    failure: null,
    roles: [],
    embedding: false,
    pinned: false,
    cloud: false,
    ctx_tokens: 32768,
    expected_ram_bytes: 1000,
    fit: "fits",
    download: null,
    instance: null,
    ...extra,
  };
}
