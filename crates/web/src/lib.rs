//! Web search for chats – only when the user turned it on (decision
//! `2026-10-02-websuche`). A planned lookup goes to the chosen provider
//! (Wikipedia or Google through Serper); pages are fetched by this computer
//! from public addresses only; the passages that answer the question are
//! chosen locally and numbered, so the answer can name its sources.

pub mod extract;
pub mod net;
pub mod providers;
pub mod rank;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ancilo_core::{Error, Result};
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

pub use providers::{Page, Query};

/// A whole lookup (search, pages, choice) may take this long.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Pages fetched for one lookup (Serper; Wikipedia brings its text).
pub const MAX_PAGES: usize = 3;
/// Passages that go to the model (≈ 1 400 tokens).
pub const MAX_PASSAGES: usize = 6;

/// Where searches go – nowhere unless the user chose a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Off,
    /// Wikipedia (free, no account).
    Wikipedia,
    /// Google results through Serper (the user's key).
    Serper,
}

/// When a chat searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Ancilo shows the search query first; nothing goes out before the user agrees.
    #[default]
    Ask,
    /// Ancilo searches by itself when a question needs it.
    Auto,
}

/// A source an answer may name as `[n]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub n: u32,
    pub title: String,
    pub url: String,
}

/// What a lookup found.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Lookup {
    pub provider: Provider,
    pub query: String,
    pub sources: Vec<Source>,
    /// The numbered passages, as the model gets them (empty: nothing found).
    pub context: String,
    /// Pages this computer fetched itself.
    #[serde(default)]
    pub fetched: Vec<String>,
    pub took_ms: u64,
}

/// Where the providers are (tests point them at fakes).
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `{lang}` is replaced by the language code.
    pub wikipedia: String,
    pub serper: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            wikipedia: "https://{lang}.wikipedia.org".into(),
            serper: "https://google.serper.dev".into(),
        }
    }
}

pub fn user_agent() -> String {
    format!(
        "Ancilo/{} (+https://github.com/stefangrunert/ancilo)",
        env!("CARGO_PKG_VERSION")
    )
}

/// The web search service: one lookup at a time.
#[derive(Clone)]
pub struct Web {
    /// For the providers (fixed addresses; the key goes only here).
    providers: Client,
    /// For pages (public addresses only).
    pages: Client,
    resolver: net::PublicResolver,
    endpoints: Endpoints,
    gate: Arc<Semaphore>,
}

impl Web {
    /// `hosts`: names the local configuration maps on purpose (tests).
    pub fn new(endpoints: Endpoints, hosts: HashMap<String, IpAddr>) -> Self {
        let resolver = net::PublicResolver::new(hosts);
        Self {
            providers: Client::builder()
                .user_agent(user_agent())
                .connect_timeout(Duration::from_secs(4))
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("provider client"),
            pages: net::page_client(resolver.clone(), &user_agent()),
            resolver,
            endpoints,
            gate: Arc::new(Semaphore::new(1)),
        }
    }

    /// Looks something up with `provider` (`key`: Serper's).
    pub async fn lookup(&self, provider: Provider, key: Option<&str>, q: &Query) -> Result<Lookup> {
        if provider == Provider::Off {
            return Err(Error::PermissionDenied(ancilo_core::msg("web.off", &[])));
        }
        if q.query.trim().is_empty() {
            return Err(Error::invalid("the search query is empty"));
        }
        let started = Instant::now();
        let work = async {
            let _one = self.gate.acquire().await.map_err(Error::internal)?;
            self.run(provider, key, q).await
        };
        let mut found = tokio::time::timeout(TOTAL_TIMEOUT, work)
            .await
            .map_err(|_| Error::unavailable(ancilo_core::msg("web.too_long", &[])))??;
        found.took_ms = started.elapsed().as_millis() as u64;
        Ok(found)
    }

    async fn run(&self, provider: Provider, key: Option<&str>, q: &Query) -> Result<Lookup> {
        let mut fetched = Vec::new();
        let pages: Vec<Page> = match provider {
            Provider::Off => Vec::new(),
            Provider::Wikipedia => {
                providers::Wikipedia {
                    client: &self.providers,
                    base: &self.endpoints.wikipedia,
                }
                .pages(q)
                .await?
            }
            Provider::Serper => {
                let key = key
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| Error::invalid(ancilo_core::msg("web.serper_key", &[])))?;
                let answer = providers::Serper {
                    client: &self.providers,
                    base: &self.endpoints.serper,
                    key,
                }
                .search(q)
                .await?;
                let top: Vec<Page> = answer.organic.iter().take(MAX_PAGES).cloned().collect();
                if !answer.boxes.is_empty() {
                    // Google already shows the answer: no page needs to be fetched.
                    answer.boxes.into_iter().chain(top).collect()
                } else {
                    // Side by side (at most MAX_PAGES).
                    let got = futures::future::join_all(top.iter().map(|p| self.page(p))).await;
                    let mut pages = Vec::new();
                    for (hit, page) in top.into_iter().zip(got) {
                        match page {
                            Some(p) => {
                                fetched.push(p.url.clone());
                                pages.push(p);
                            }
                            None => pages.push(hit),
                        }
                    }
                    pages
                }
            }
        };
        Ok(numbered(provider, q, pages, fetched))
    }

    /// A result page as readable text, its snippet first (None: not fetched).
    async fn page(&self, hit: &Page) -> Option<Page> {
        let f = match net::fetch(&self.pages, &self.resolver, &hit.url).await {
            Ok(f) => f,
            Err(why) => {
                tracing::debug!(url = %hit.url, %why, "page not fetched");
                return None;
            }
        };
        let url = f.url.to_string();
        let (title, text) =
            tokio::task::spawn_blocking(move || extract::readable(&f.body, f.url.as_str(), f.html))
                .await
                .ok()??;
        Some(Page {
            title: if title.is_empty() {
                hit.title.clone()
            } else {
                title
            },
            url,
            text: format!("{}\n\n{text}", hit.text),
        })
    }
}

/// Chooses passages and numbers the pages they come from.
fn numbered(provider: Provider, q: &Query, pages: Vec<Page>, fetched: Vec<String>) -> Lookup {
    let texts: Vec<String> = pages.iter().map(|p| p.text.clone()).collect();
    let ask = format!("{} {}", q.query, q.topic.as_deref().unwrap_or_default());
    let chosen = rank::choose(&texts, &ask, MAX_PASSAGES);
    let mut sources: Vec<Source> = Vec::new();
    let mut numbers: HashMap<usize, u32> = HashMap::new();
    let mut blocks: Vec<(u32, Vec<String>)> = Vec::new();
    for c in chosen {
        let page = &pages[c.page];
        let n = *numbers.entry(c.page).or_insert_with(|| {
            // Pages with the same address share a number.
            match sources
                .iter()
                .find(|s| !page.url.is_empty() && s.url == page.url)
            {
                Some(s) => s.n,
                None => {
                    let n = sources.len() as u32 + 1;
                    sources.push(Source {
                        n,
                        title: page.title.clone(),
                        url: page.url.clone(),
                    });
                    n
                }
            }
        });
        match blocks.iter_mut().find(|(m, _)| *m == n) {
            Some((_, texts)) => texts.push(c.text),
            None => blocks.push((n, vec![c.text])),
        }
    }
    blocks.sort_by_key(|(n, _)| *n);
    let context = blocks
        .iter()
        .map(|(n, texts)| {
            let s = &sources[*n as usize - 1];
            format!("[{n}] {}\n{}", s.title, texts.join("\n…\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Lookup {
        provider,
        query: q.query.clone(),
        sources,
        context,
        fetched,
        took_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_numbered_once_and_passages_grouped() {
        let pages = vec![
            Page { title: "Oslo".into(), url: "https://w/Oslo".into(), text: "Oslo ist die Hauptstadt Norwegens.\n== Bevölkerung ==\nOslo hat 728.714 Einwohner.".into() },
            Page { title: "Oslo (Tettsted)".into(), url: "https://w/Tettsted".into(), text: "Das Stadtgebiet Oslo hat 1.119.478 Einwohner.".into() },
            Page { title: "Fußball".into(), url: "https://w/Fussball".into(), text: "Ein Ballsport.".into() },
        ];
        let q = Query {
            query: "Einwohnerzahl Oslo".into(),
            topic: Some("Oslo".into()),
            lang: "de".into(),
        };
        let l = numbered(Provider::Wikipedia, &q, pages, vec![]);
        assert_eq!(
            l.sources
                .iter()
                .map(|s| s.title.as_str())
                .collect::<Vec<_>>(),
            ["Oslo", "Oslo (Tettsted)", "Fußball"][..l.sources.len()].to_vec()
        );
        assert!(
            l.context.starts_with("[1] Oslo\nOslo ist die Hauptstadt"),
            "{}",
            l.context
        );
        assert!(l.context.contains("728.714"));
        assert!(l.context.contains("[2] Oslo (Tettsted)\nDas Stadtgebiet"));
    }

    #[tokio::test]
    async fn nothing_goes_out_while_it_is_off() {
        let web = Web::new(
            Endpoints {
                wikipedia: "http://127.0.0.1:9/{lang}".into(),
                serper: "http://127.0.0.1:9".into(),
            },
            HashMap::new(),
        );
        let q = Query {
            query: "Oslo".into(),
            topic: None,
            lang: "de".into(),
        };
        let err = web.lookup(Provider::Off, None, &q).await.unwrap_err();
        assert_eq!(err.code(), "permission_denied");
        let err = web.lookup(Provider::Serper, None, &q).await.unwrap_err();
        assert!(err.message().contains("needs a key"), "{}", err.message());
    }
}
