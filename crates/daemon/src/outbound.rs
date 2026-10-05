//! The user's log of what left this computer – System › What left this Mac.
//!
//! Every request that leaves goes through `ancilo_net::Net` and lands here:
//! web searches and pages, Hugging Face, model and llama.cpp downloads, the
//! model catalog, messages to cloud models – and, from the app, update
//! checks. Kept 30 days, on this computer only; the user can empty it.
//!
//! Its operations are the user's own (`OpBuilder::own`): neither an MCP
//! client nor the assistant sees them – the log holds search terms and what
//! was sent to cloud models.

use std::collections::BTreeMap;

use ancilo_core::{Error, EventBus, NoInput, OpBuilder, Registry, Result};
use ancilo_net::{By, Departure, Purpose, Recorder};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::params;
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// How long entries are kept.
pub const KEEP_DAYS: i64 = 30;

/// The log in the database.
#[derive(Clone)]
pub struct Log {
    db: Db,
    bus: EventBus,
}

impl Log {
    pub fn new(db: Db, bus: EventBus) -> Self {
        let log = Self { db, bus };
        log.prune();
        log
    }

    /// Entries older than [`KEEP_DAYS`] go.
    pub fn prune(&self) {
        let before = (Utc::now() - Duration::days(KEEP_DAYS)).to_rfc3339();
        if let Err(e) = self
            .db
            .with(|c| c.execute("DELETE FROM outbound WHERE at < ?1", params![before]))
        {
            tracing::warn!(error = %e.message(), "pruning the outbound log failed");
        }
    }

    fn page(&self, before: Option<i64>, limit: u32) -> Result<Vec<Entry>> {
        let rows: Vec<(i64, String)> = self.db.with(|c| {
            let mut s = c.prepare(
                "SELECT id, departure FROM outbound WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = s.query_map(params![before.unwrap_or(i64::MAX), limit], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            rows.collect()
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, d)| {
                Some(Entry {
                    id,
                    departure: serde_json::from_str(&d).ok()?,
                })
            })
            .collect())
    }

    fn summary(&self) -> Result<Summary> {
        let today = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc())
            .unwrap_or_else(Utc::now);
        let week = Utc::now() - Duration::days(7);
        let count = |since: DateTime<Utc>| -> Result<BTreeMap<String, u64>> {
            self.db.with(|c| {
                let mut s = c.prepare(
                    "SELECT purpose, COUNT(*) FROM outbound WHERE at >= ?1 GROUP BY purpose",
                )?;
                let rows = s.query_map(params![since.to_rfc3339()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
                })?;
                rows.collect()
            })
        };
        let (first, last): (Option<String>, Option<String>) = self.db.with(|c| {
            c.query_row("SELECT MIN(at), MAX(at) FROM outbound", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        })?;
        let parse = |s: Option<String>| {
            s.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc))
        };
        Ok(Summary {
            today: count(today)?,
            week: count(week)?,
            first: parse(first),
            last: parse(last),
            keep_days: KEEP_DAYS,
        })
    }

    fn clear(&self) -> Result<u64> {
        Ok(self.db.with(|c| c.execute("DELETE FROM outbound", []))? as u64)
    }
}

impl Recorder for Log {
    fn record(&self, d: Departure) {
        let Ok(json) = serde_json::to_string(&d) else {
            return;
        };
        let purpose = serde_json::to_value(d.purpose)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        let at = d.at.to_rfc3339();
        match self.db.with(|c| {
            c.execute(
                "INSERT INTO outbound (at, purpose, departure) VALUES (?1, ?2, ?3)",
                params![at, purpose, json],
            )
        }) {
            Ok(_) => {
                self.bus.emit(
                    "outbound.departed",
                    None,
                    json!({"purpose": d.purpose, "host": d.host}),
                );
            }
            Err(e) => tracing::warn!(error = %e.message(), "the outbound log could not be written"),
        }
    }
}

/// One entry: an id (for paging) and what left.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Entry {
    pub id: i64,
    #[serde(flatten)]
    pub departure: Departure,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// More, older entries exist (ask with `before` = the last id).
    pub more: bool,
}

/// Counts per purpose – today and in the last 7 days.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Summary {
    pub today: BTreeMap<String, u64>,
    pub week: BTreeMap<String, u64>,
    /// The oldest entry kept, and the newest.
    pub first: Option<DateTime<Utc>>,
    pub last: Option<DateTime<Utc>>,
    pub keep_days: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageInput {
    /// Entries older than this id (the next page).
    #[serde(default)]
    pub before: Option<i64>,
    /// How many (default 50, at most 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// What the app itself sent (its update check and download – the updater
/// runs in the app, not in the daemon).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppDeparture {
    /// `update_check` or `update_download`.
    pub purpose: Purpose,
    pub subject: String,
    pub by: By,
    pub url: String,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub received_bytes: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn register(registry: &mut Registry, log: Log) {
    let l = log.clone();
    registry.register(
        OpBuilder::new("outbound_log")
            .summary("What left this computer: web searches, downloads, messages to cloud models – newest first")
            .description("The user's log of every request that left this computer (kept 30 days): when, why, to whom and what was sent. Only for the user – not for models.")
            .own()
            .handler(move |_ctx, i: PageInput| {
                let l = l.clone();
                async move {
                    l.prune();
                    let limit = i.limit.unwrap_or(50).clamp(1, 200);
                    let mut entries = l.page(i.before, limit + 1)?;
                    let more = entries.len() > limit as usize;
                    entries.truncate(limit as usize);
                    Ok(Page { entries, more })
                }
            }),
    );
    let l = log.clone();
    registry.register(
        OpBuilder::new("outbound_summary")
            .summary("How much left this computer today and this week, by purpose")
            .own()
            .handler(move |_ctx, _i: NoInput| {
                let l = l.clone();
                async move {
                    l.prune();
                    l.summary()
                }
            }),
    );
    let l = log.clone();
    registry.register(
        OpBuilder::new("clear_outbound_log")
            .summary("Empty the log of what left this computer")
            .manage()
            .consequential()
            .own()
            .handler(move |_ctx, _i: NoInput| {
                let l = l.clone();
                async move { Ok(json!({"removed": l.clear()?})) }
            }),
    );
    let l = log;
    registry.register(
        OpBuilder::new("record_departure")
            .summary("Log what the app itself sent (its update check and download)")
            .manage()
            .own()
            .handler(move |_ctx, i: AppDeparture| {
                let l = l.clone();
                async move {
                    if !matches!(i.purpose, Purpose::UpdateCheck | Purpose::UpdateDownload) {
                        return Err(Error::invalid(
                            "the app logs only its update check and download",
                        ));
                    }
                    let url = reqwest_free_host(&i.url);
                    l.record(Departure {
                        at: Utc::now(),
                        purpose: i.purpose,
                        subject: i.subject,
                        by: i.by,
                        method: "GET".into(),
                        host: url,
                        url: i.url,
                        sent: None,
                        sent_bytes: 0,
                        received_bytes: i.received_bytes,
                        redirected_to: None,
                        status: i.status,
                        error: i.error,
                    });
                    Ok(json!({"logged": true}))
                }
            }),
    );
}

/// The host of an address (`https://github.com/x` → `github.com`).
fn reqwest_free_host(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn departure(purpose: Purpose, days_ago: i64) -> Departure {
        Departure {
            at: Utc::now() - Duration::days(days_ago),
            purpose,
            subject: "Gaustatoppen".into(),
            by: By::You,
            method: "GET".into(),
            host: "de.wikipedia.org".into(),
            url: "https://de.wikipedia.org/w/api.php?srsearch=Gaustatoppen".into(),
            sent: None,
            sent_bytes: 0,
            received_bytes: Some(1200),
            redirected_to: None,
            status: Some(200),
            error: None,
        }
    }

    #[test]
    fn the_log_keeps_30_days_pages_and_counts() {
        let db = Db::in_memory().unwrap();
        let log = Log::new(db.clone(), EventBus::in_memory());
        for i in 0..3 {
            log.record(departure(Purpose::WebSearch, 0));
            log.record(departure(Purpose::ModelDownload, i + 2));
        }
        log.record(departure(Purpose::CloudModel, 40));
        // Older than 30 days: gone with the next start.
        let log = Log::new(db, EventBus::in_memory());
        let all = log.page(None, 100).unwrap();
        assert_eq!(all.len(), 6);
        assert!(all.windows(2).all(|w| w[0].id > w[1].id), "newest first");
        let next = log.page(Some(all[1].id), 2).unwrap();
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].id, all[2].id);
        let s = log.summary().unwrap();
        assert_eq!(s.today.get("web_search"), Some(&3));
        assert_eq!(s.today.get("model_download"), None);
        assert_eq!(s.week.get("model_download"), Some(&3));
        assert_eq!(log.clear().unwrap(), 6);
        assert!(log.page(None, 10).unwrap().is_empty());
    }

    #[test]
    fn hosts_are_read_from_addresses() {
        assert_eq!(
            reqwest_free_host(
                "https://github.com/stefangrunert/ancilo/releases/latest/download/latest.json"
            ),
            "github.com"
        );
        assert_eq!(
            reqwest_free_host("https://user@example.org:8443/x?y"),
            "example.org:8443"
        );
    }
}
