//! System operations: `diagnose` (what is going on?) and `setup` (the first
//! start: a chat model that fits and an embedding model).

use std::sync::Arc;

use ancilo_core::{Config, NoInput, OpBuilder, Registry, Result};
use ancilo_gateway::Gateway;
use ancilo_models::ModelManager;
use ancilo_models::catalog::Purpose;
use ancilo_models::manager::{ModelStatus, ModelView, PlanPreview};
use ancilo_models::planner::{Fit, Wish};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Serialize, JsonSchema)]
pub struct Problem {
    pub ts: String,
    pub kind: String,
    pub subject: Option<String>,
    pub data: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Diagnosis {
    pub version: String,
    pub hardware: Value,
    pub models: Vec<Value>,
    /// Recent failures and crashes (newest first).
    pub recent_problems: Vec<Problem>,
    /// Model calls of the last hour.
    pub calls_last_hour: Value,
    pub config: Value,
    /// What stands out, in plain words.
    pub hints: Vec<String>,
}

fn recent_problems(db: &Db) -> Vec<Problem> {
    db.with(|c| {
        let mut s = c.prepare(
            "SELECT ts, kind, subject, data FROM events
             WHERE kind LIKE '%failed%' OR kind LIKE '%crash%' OR kind LIKE '%error%'
             ORDER BY seq DESC LIMIT 20",
        )?;
        let rows = s.query_map(params![], |r| {
            Ok(Problem {
                ts: r.get(0)?,
                kind: r.get(1)?,
                subject: r.get(2)?,
                data: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
            })
        })?;
        rows.collect()
    })
    .unwrap_or_default()
}

fn calls_last_hour(db: &Db) -> Value {
    db.with(|c| {
        c.query_row(
            "SELECT count(*), avg(latency_ms), sum(outcome IN ('error', 'failed')), sum(load_ms > 0)
             FROM inference_log WHERE ts > datetime('now', '-1 hour')",
            [],
            |r| {
                Ok(json!({
                    "requests": r.get::<_, i64>(0)?,
                    "avg_latency_ms": r.get::<_, Option<f64>>(1)?.map(|v| v.round()),
                    "errors": r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    "with_model_load": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                }))
            },
        )
    })
    .unwrap_or(Value::Null)
}

pub async fn diagnose(manager: &ModelManager, db: &Db, config: &Config) -> Result<Diagnosis> {
    let hw = manager.hardware_view().await?;
    let models = manager.list().await?;
    let mut hints = Vec::new();
    for m in &models {
        if matches!(
            m.status,
            ModelStatus::Crashed | ModelStatus::Failed | ModelStatus::DownloadFailed
        ) {
            hints.push(format!(
                "{} is {:?}{} – details: `ancilo logs {}`",
                m.id,
                m.status,
                m.failure
                    .as_deref()
                    .map(|f| format!(" ({f})"))
                    .unwrap_or_default(),
                m.id
            ));
        }
    }
    if models.is_empty() {
        hints.push("no model yet – `ancilo setup` picks one that fits this machine".into());
    }
    if manager.embedding_model().is_none() {
        hints.push("no embedding model: search is full text only".into());
    }
    if hw.model_budget_bytes > 0 && hw.used_bytes * 10 > hw.model_budget_bytes * 9 {
        hints.push(
            "memory for models is nearly full – loading another model unloads one first".into(),
        );
    }
    let calls = calls_last_hour(db);
    if calls["with_model_load"].as_i64().unwrap_or(0) > 3 {
        hints.push("models were loaded repeatedly in the last hour – pin the models you use most (`set_pinned`) or use fewer at once".into());
    }
    Ok(Diagnosis {
        version: ancilo_core::VERSION.into(),
        hardware: serde_json::to_value(&hw)?,
        models: models
            .iter()
            .map(|m| json!({"id": m.id, "status": m.status, "roles": m.roles, "failure": m.failure, "cloud": m.cloud, "pinned": m.pinned}))
            .collect(),
        recent_problems: recent_problems(db),
        calls_last_hour: calls,
        config: json!({
            "port": config.port,
            "ram_reserve_gib": config.ram_reserve_gib,
            "model_search_dirs": config.model_search_dirs,
            "custom_llama_server": config.llama_server_bin.is_some(),
            "model_aliases": config.model_aliases,
        }),
        hints,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupInput {
    /// Only show what would be installed.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetupPlan {
    /// The chat model chosen for this machine (none if one exists already).
    pub chat: Option<PlanPreview>,
    pub embed: Option<PlanPreview>,
    /// Bytes to download (files already on disk are reused).
    pub download_bytes: u64,
    pub notes: Vec<String>,
    /// Added models (not with `dry_run`).
    pub added: Vec<ModelView>,
}

fn download_size(p: &PlanPreview) -> u64 {
    p.download_bytes
}

pub async fn setup(manager: &ModelManager, input: SetupInput) -> Result<SetupPlan> {
    // The same choice the app offers: the catalog's best fit for this machine.
    let rec = manager.recommend(&[Purpose::Chat], false).await?;
    let (catalog, _) = manager.catalog(false).await;
    let embed_address = catalog
        .models
        .iter()
        .find(|m| m.kind == "embedding")
        .map(|m| m.address.clone());
    let models = manager.list().await?;
    let mut notes = Vec::new();
    let has_chat = models.iter().any(|m| !m.embedding && !m.cloud);
    let mut chat = None;
    if has_chat {
        notes.push("a chat model is already installed – kept".into());
    } else {
        if rec.too_big > 0 {
            notes.push(format!(
                "{} larger models from Ancilo's list need more memory than this machine has",
                rec.too_big
            ));
        }
        let candidates = rec
            .best
            .iter()
            .chain(&rec.alternatives)
            .chain(&rec.more)
            .map(|s| s.address.clone());
        for address in candidates {
            match manager.plan(&address, &Wish::default()).await {
                Ok(p) if p.plan.fit == Fit::Fits => {
                    chat = Some(p);
                    break;
                }
                Ok(p) => notes.push(format!("{address}: {:?} – {}", p.plan.fit, p.plan.reason)),
                Err(e) => notes.push(format!("{address}: {}", e.message())),
            }
        }
        if chat.is_none() {
            notes.push("no recommended chat model fits comfortably – choose one with `ancilo plan <address>`".into());
        }
    }
    let mut embed = None;
    if manager.embedding_model().is_none()
        && let Some(address) = &embed_address
    {
        match manager.plan(address, &Wish::default()).await {
            Ok(p) => embed = Some(p),
            Err(e) => notes.push(format!("{address}: {}", e.message())),
        }
    }
    let download_bytes = chat.iter().chain(embed.iter()).map(download_size).sum();
    let mut added = Vec::new();
    if !input.dry_run {
        for (preview, start) in [(&embed, false), (&chat, true)] {
            if let Some(p) = preview {
                let address = p.repo.as_ref().map_or_else(
                    || p.plan.primary_file().path.clone(),
                    |r| format!("hf.co/{}", r.id),
                );
                let wish = Wish {
                    quant: p.plan.quant.clone(),
                    ..Wish::default()
                };
                added.push(manager.add(&address, &wish, start).await?);
            }
        }
    }
    Ok(SetupPlan {
        chat,
        embed,
        download_bytes,
        notes,
        added,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsInput {
    pub model: String,
    /// Last lines (default 50, at most 500).
    #[serde(default)]
    pub lines: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Logs {
    pub model: String,
    pub lines: Vec<String>,
}

pub fn register(
    registry: &mut Registry,
    manager: ModelManager,
    gateway: Gateway,
    db: Db,
    config: Config,
    logs_dir: std::path::PathBuf,
) {
    let _ = gateway;
    let (m, d, c) = (manager.clone(), db, Arc::new(config));
    registry.register(
        OpBuilder::new("diagnose")
            .summary("Diagnose Ancilo: hardware, models, recent errors, load and configuration – with hints")
            .handler(move |_ctx, _i: NoInput| {
                let (m, d, c) = (m.clone(), d.clone(), c.clone());
                async move { diagnose(&m, &d, &c).await }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("setup")
            .summary("First start: add a chat model that fits this machine and an embedding model")
            .description("Chooses the best recommended chat model that fits comfortably and the embedding model for search, shows the download size (dry_run), and adds them. Models that already exist are kept.")
            .manage()
            .consequential()
            .handler(move |_ctx, i: SetupInput| {
                let m = m.clone();
                async move { setup(&m, i).await }
            }),
    );
    registry.register(
        OpBuilder::new("choose_folder")
            .summary("Let the user pick a folder in the system's dialog (macOS)")
            .handler(move |_ctx, i: ChooseInput| async move { choose_folder(i).await }),
    );
    let m = manager;
    registry.register(
        OpBuilder::new("model_logs")
            .summary("The last lines of a model's log (llama.cpp output)")
            .handler(move |_ctx, i: LogsInput| {
                let (m, dir) = (m.clone(), logs_dir.clone());
                async move {
                    let id = m.find(&i.model)?.id;
                    let text =
                        std::fs::read_to_string(dir.join(format!("{id}.log"))).unwrap_or_default();
                    let n = i.lines.unwrap_or(50).min(500);
                    let all: Vec<&str> = text.lines().collect();
                    Ok(Logs {
                        model: id,
                        lines: all[all.len().saturating_sub(n)..]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    })
                }
            }),
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChooseInput {
    /// Shown in the dialog (required: the dialog opens on the user's screen,
    /// so every caller says why).
    pub prompt: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Chosen {
    /// `None`: the user cancelled.
    pub path: Option<std::path::PathBuf>,
}

/// AppleScript for the folder dialog, shown in front of the user's current app.
fn choose_script(prompt: &str) -> String {
    let prompt = prompt.replace('\\', "").replace('"', "'");
    format!(
        "tell application (path to frontmost application as text) to POSIX path of (choose folder with prompt \"{prompt}\")"
    )
}

async fn choose_folder(i: ChooseInput) -> Result<Chosen> {
    if !cfg!(target_os = "macos") {
        return Err(ancilo_core::Error::Unavailable(
            "a folder dialog is only available on macOS – type the path".into(),
        ));
    }
    if i.prompt.trim().is_empty() {
        return Err(ancilo_core::Error::invalid(
            "say what the folder is for (prompt)",
        ));
    }
    let script = choose_script(&i.prompt);
    // Nobody answers? The dialog closes after five minutes.
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        tokio::process::Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        ancilo_core::Error::Conflict("no folder was chosen within five minutes".into())
    })??;
    if !out.status.success() {
        // "User canceled." (-128) is an answer, not an error.
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") {
            return Ok(Chosen { path: None });
        }
        return Err(ancilo_core::Error::internal(format!(
            "folder dialog: {}",
            err.trim()
        )));
    }
    let path = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_end_matches('/')
        .to_string();
    Ok(Chosen {
        path: (!path.is_empty()).then(|| path.into()),
    })
}

#[cfg(test)]
mod choose_tests {
    #[test]
    fn the_dialog_prompt_cannot_break_out_of_the_script() {
        let s = super::choose_script(r#"x" & do shell script "rm -rf ~" & ""#);
        assert_eq!(s.matches('"').count(), 2, "{s}");
        assert!(s.starts_with("tell application (path to frontmost application as text) to POSIX path of (choose folder with prompt \""));
    }
}
