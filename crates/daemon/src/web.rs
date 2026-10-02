//! Web search settings and lookups (decision `2026-10-02-websuche`): off
//! until the user picks a provider; the Serper key lives in the keychain only.

use std::sync::Arc;

use ancilo_core::secrets::{SecretStore, mask};
use ancilo_core::{Config, Error, EventBus, NoInput, OpBuilder, Registry, Result};
use ancilo_storage::Db;
use ancilo_web::{Endpoints, Lookup, Mode, Provider, Query, Web};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const KEY: &str = "web_search";
const SERPER_SECRET: &str = "web.serper";

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WebSettings {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub mode: Mode,
}

/// The settings as shown – the key only masked.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WebSearchView {
    pub provider: Provider,
    pub mode: Mode,
    /// The Serper key, masked (`…3f9a`), if one is saved.
    pub serper_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetWebSearch {
    #[serde(default)]
    pub provider: Option<Provider>,
    #[serde(default)]
    pub mode: Option<Mode>,
    /// The Serper key (kept in the keychain); empty removes it.
    #[serde(default)]
    pub serper_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestWebSearch {
    /// The provider to try (default: the chosen one).
    #[serde(default)]
    pub provider: Option<Provider>,
    /// A Serper key to try before saving it.
    #[serde(default)]
    pub serper_key: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Tested {
    pub provider: Provider,
    pub sources: usize,
    pub took_ms: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebSearchInput {
    /// A short search query (it goes to the provider).
    pub query: String,
    /// The subject as an article title (helps Wikipedia).
    #[serde(default)]
    pub topic: Option<String>,
    /// Two-letter language code (default `en`).
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Clone)]
pub struct WebSearch {
    web: Web,
    db: Db,
    secrets: Arc<dyn SecretStore>,
    bus: EventBus,
}

fn valid_key(k: &str) -> Result<&str> {
    let k = k.trim();
    if k.len() > 200 || k.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::invalid(
            "this does not look like a Serper key – copy it again from serper.dev",
        ));
    }
    Ok(k)
}

impl WebSearch {
    pub fn new(config: &Config, db: Db, secrets: Arc<dyn SecretStore>, bus: EventBus) -> Self {
        let endpoints = Endpoints {
            wikipedia: config.wikipedia_endpoint.clone(),
            serper: config.serper_endpoint.clone(),
        };
        let hosts = config
            .web_hosts
            .iter()
            .map(|(h, ip)| (h.to_ascii_lowercase(), *ip))
            .collect();
        Self {
            web: Web::new(endpoints, hosts),
            db,
            secrets,
            bus,
        }
    }

    pub fn settings(&self) -> WebSettings {
        self.db
            .get_setting(KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn key(&self) -> Result<Option<String>> {
        self.secrets.get(SERPER_SECRET)
    }

    pub fn view(&self) -> Result<WebSearchView> {
        let s = self.settings();
        Ok(WebSearchView {
            provider: s.provider,
            mode: s.mode,
            serper_key: self.key()?.as_deref().map(mask),
        })
    }

    pub fn set(&self, input: SetWebSearch) -> Result<WebSearchView> {
        let mut s = self.settings();
        if let Some(k) = &input.serper_key {
            if k.trim().is_empty() {
                self.secrets.delete(SERPER_SECRET)?;
                // Without its key Serper cannot search.
                if s.provider == Provider::Serper && input.provider.is_none() {
                    s.provider = Provider::Off;
                }
            } else {
                self.secrets.set(SERPER_SECRET, valid_key(k)?)?;
            }
        }
        if let Some(p) = input.provider {
            if p == Provider::Serper && self.key()?.is_none() {
                return Err(Error::invalid("add your Serper key first"));
            }
            s.provider = p;
        }
        if let Some(m) = input.mode {
            s.mode = m;
        }
        self.db.set_setting(KEY, &serde_json::to_string(&s)?)?;
        self.bus.emit(
            "web.changed",
            None,
            json!({"provider": s.provider, "mode": s.mode}),
        );
        self.view()
    }

    /// Looks something up with the chosen provider (refused while it is off).
    pub async fn lookup(&self, q: &Query) -> Result<Lookup> {
        let provider = self.settings().provider;
        let key = self.key()?;
        self.web.lookup(provider, key.as_deref(), q).await
    }

    /// One real search to see that a provider works (a key before saving it).
    pub async fn test(&self, input: TestWebSearch) -> Result<Tested> {
        let provider = input.provider.unwrap_or(self.settings().provider);
        let key = match input.serper_key.as_deref() {
            Some(k) if !k.trim().is_empty() => Some(valid_key(k)?.to_string()),
            _ => self.key()?,
        };
        let q = Query {
            query: "Wikipedia".into(),
            topic: Some("Wikipedia".into()),
            lang: "en".into(),
        };
        let found = self.web.lookup(provider, key.as_deref(), &q).await?;
        if found.sources.is_empty() {
            return Err(Error::unavailable(
                "the search worked, but found nothing – try again later",
            ));
        }
        Ok(Tested {
            provider,
            sources: found.sources.len(),
            took_ms: found.took_ms,
        })
    }
}

pub fn register(registry: &mut Registry, ws: WebSearch) {
    let w = ws.clone();
    registry.register(
        OpBuilder::new("get_web_search")
            .summary("Whether chats may search the web, with which provider, and when")
            .handler(move |_ctx, _i: NoInput| {
                let w = w.clone();
                async move { w.view() }
            }),
    );
    let w = ws.clone();
    registry.register(
        OpBuilder::new("set_web_search")
            .summary("Choose the web search provider (off, wikipedia, serper), when chats search (ask, auto), or set the Serper key")
            .description("Off by default. `wikipedia` needs no account; `serper` (Google results) needs the user's key from serper.dev, kept in the system keychain. `mode`: `ask` shows the search query first and searches only after the user agrees; `auto` searches when a question needs it. Search queries go to the chosen provider; result pages are fetched by this computer.")
            .manage()
            .handler(move |_ctx, i: SetWebSearch| {
                let w = w.clone();
                async move { w.set(i) }
            }),
    );
    let w = ws.clone();
    registry.register(
        OpBuilder::new("test_web_search")
            .summary("Try a web search provider once (a Serper key before saving it)")
            .manage()
            .handler(move |_ctx, i: TestWebSearch| {
                let w = w.clone();
                async move { w.test(i).await }
            }),
    );
    let w = ws;
    registry.register(
        OpBuilder::new("web_search")
            .summary("Search the web with the chosen provider – numbered passages and their sources")
            .description("Refused while web search is off. The query goes to the provider (Wikipedia or Google through Serper); result pages are fetched by this computer from public addresses only; the passages that fit are chosen locally. Pass a short query, not private data.")
            .handler(move |_ctx, i: WebSearchInput| {
                let w = w.clone();
                async move {
                    w.lookup(&Query {
                        query: i.query,
                        topic: i.topic,
                        lang: i.lang.unwrap_or_else(|| "en".into()),
                    })
                    .await
                }
            }),
    );
}
