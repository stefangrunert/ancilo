import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Client, readToken } from "./api/client";
import { App } from "./App";
import { I18nProvider, detectLang, translate } from "./i18n";
import { ClientProvider } from "./state/store";
import "./styles.css";

const root = createRoot(document.getElementById("root")!);
const token = readToken();
const base = window.__ANCILO__?.url ?? window.location.origin;
document.documentElement.lang = detectLang();

if (!token) {
  const lang = detectLang();
  root.render(
    <main className="app">
      <h1>{translate(lang, "status.noToken")}</h1>
      <p>{translate(lang, "status.noTokenHint")}</p>
    </main>,
  );
} else {
  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 5_000, refetchOnWindowFocus: false } } });
  root.render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <ClientProvider client={new Client(base, token)} queryClient={queryClient}>
          <I18nProvider>
            <App />
          </I18nProvider>
        </ClientProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
}
