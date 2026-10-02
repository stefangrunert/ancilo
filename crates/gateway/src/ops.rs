//! Gateway operations.

use std::collections::BTreeMap;

use ancilo_core::{Error, NoInput, OpBuilder, Registry, Result};
use ancilo_storage::rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Gateway;
use crate::reliability::{ReliabilityConfig, Stage};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatsInput {
    /// Only this model.
    #[serde(default)]
    pub model: Option<String>,
    /// Only the last N minutes (default: all).
    #[serde(default)]
    pub minutes: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Stats {
    pub requests: u64,
    pub with_tools: u64,
    /// Count per outcome: ok, repaired, retried, constrained, failed, text, error.
    pub outcomes: BTreeMap<String, u64>,
    /// How often each intervention happened (repair:json, retry:1, …).
    pub interventions: BTreeMap<String, u64>,
    pub avg_latency_ms: f64,
    pub avg_queue_ms: f64,
    pub avg_tokens_per_sec: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetReliability {
    /// `all`, `off`, or a list of stages: prompt, constrained, validate, repair,
    /// retry, nudge. With `model`: `default` removes the model's own setting.
    pub stages: Value,
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// Only for this model (otherwise the global default).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReliabilitySettings {
    /// Used by every model without its own setting.
    pub global: ReliabilityConfig,
    /// Models with their own setting.
    pub models: BTreeMap<String, ReliabilityConfig>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuneInput {
    /// Model id, name or role.
    pub model: String,
    /// Runs per task (default 1).
    #[serde(default)]
    pub repeat: Option<u32>,
    /// Only measure, do not change the setting.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TuneCandidate {
    pub stages: String,
    pub tool_calling: f64,
    pub no_tool: f64,
    /// Mean latency on the tool-calling and the no-tool set.
    pub latency_mean_ms: [u64; 2],
    /// Not below `off` on the no-tool set and within the latency budget.
    pub eligible: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TuneReport {
    pub model: String,
    pub candidates: Vec<TuneCandidate>,
    pub chosen: String,
    pub applied: bool,
    pub rule: String,
}

/// Candidate pipelines for `tune_reliability`: all stages, without the
/// prompt hint, without nudge, none. (Stages that only act on invalid calls –
/// constrained, validate, repair, retry – are not ablated: they cannot make a
/// valid answer worse.) Order = preference on complete ties.
pub const TUNE_CANDIDATES: &[&str] = &[
    "all",
    "constrained,validate,repair,retry,nudge",
    "prompt,constrained,validate,repair,retry",
    "off",
];

/// Latency budget per set: mean at most 1.5 × `off` + 0.5 s.
pub fn within_latency_budget(candidate_ms: u64, off_ms: u64) -> bool {
    candidate_ms as f64 <= off_ms as f64 * 1.5 + 500.0
}

/// Picks the candidate with the best tool-calling rate among the eligible
/// ones; ties: better no-tool rate, then lower total mean latency, then order.
pub fn choose(candidates: &[TuneCandidate]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, c) in candidates.iter().enumerate() {
        if !c.eligible {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let o = &candidates[b];
                let lat = |x: &TuneCandidate| x.latency_mean_ms[0] + x.latency_mean_ms[1];
                let same = |a: f64, b: f64| (a - b).abs() < 1e-9;
                c.tool_calling > o.tool_calling + 1e-9
                    || (same(c.tool_calling, o.tool_calling) && c.no_tool > o.no_tool + 1e-9)
                    || (same(c.tool_calling, o.tool_calling)
                        && same(c.no_tool, o.no_tool)
                        && lat(c) < lat(o))
            }
        };
        if better {
            best = Some(i);
        }
    }
    best
}

async fn tune(
    gw: &Gateway,
    base_url: &str,
    token: &str,
    dir: &std::path::Path,
    i: TuneInput,
) -> Result<TuneReport> {
    let model = gw.manager().resolve_strict(&i.model)?;
    let mut candidates = Vec::new();
    for stages in TUNE_CANDIDATES {
        let mut rates = Vec::new();
        let mut means = [0u64; 2];
        for (n, suite) in ["tool-calling", "no-tool"].iter().enumerate() {
            let r = run_eval(
                gw,
                base_url,
                token,
                dir,
                EvalInput {
                    suite: (*suite).into(),
                    model: Some(model.clone()),
                    reliability: Some((*stages).into()),
                    repeat: Some(i.repeat.unwrap_or(1)),
                },
            )
            .await?;
            // A broken measurement must not pick (and persist) a winner.
            if r.errors > 0 {
                return Err(Error::unavailable(format!(
                    "measurement incomplete: {} requests failed on {suite} with `{stages}` – setting unchanged",
                    r.errors
                )));
            }
            means[n] = r.latency_mean_ms;
            rates.push(r.success_rate);
        }
        candidates.push(TuneCandidate {
            stages: (*stages).into(),
            tool_calling: rates[0],
            no_tool: rates[1],
            latency_mean_ms: means,
            eligible: true,
        });
    }
    let off = candidates
        .iter()
        .find(|c| c.stages == "off")
        .map(|c| (c.no_tool, c.latency_mean_ms))
        .unwrap_or((0.0, [0, 0]));
    for c in &mut candidates {
        c.eligible = c.no_tool + 1e-9 >= off.0
            && within_latency_budget(c.latency_mean_ms[0], off.1[0])
            && within_latency_budget(c.latency_mean_ms[1], off.1[1]);
    }
    let chosen = choose(&candidates)
        .map(|b| candidates[b].stages.clone())
        .unwrap_or_else(|| "all".into());
    if !i.dry_run {
        gw.set_model_reliability(
            &model,
            Some(ReliabilityConfig::parse(&chosen).map_err(Error::invalid)?),
        )?;
    }
    Ok(TuneReport {
        model,
        candidates,
        chosen,
        applied: !i.dry_run,
        rule: "best tool-calling rate among the pipelines that cause no more unwanted tool calls (no-tool set) than no pipeline and stay within 1.5 × its mean latency + 0.5 s; ties: better no-tool rate, then faster".into(),
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalInput {
    /// Built-in suite (`tool-calling`, `sample`) or path to a YAML suite.
    pub suite: String,
    /// Model id or role. Default: the default model.
    #[serde(default)]
    pub model: Option<String>,
    /// Reliability stages: `all`, `off`, `model` (the model's own setting) or
    /// a comma-separated list. Default: `all`.
    #[serde(default)]
    pub reliability: Option<String>,
    /// Runs per task. Default: 3.
    #[serde(default)]
    pub repeat: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiConfigInput {
    /// `claude_code`, `codex` or `openai` (any OpenAI-compatible tool).
    pub client: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApiConfig {
    pub base_url: String,
    /// Environment variables to set.
    pub env: BTreeMap<String, String>,
    /// Configuration file snippet, if the client needs one.
    pub config: Option<String>,
    pub notes: String,
}

pub fn stats(gw: &Gateway, input: &StatsInput) -> Result<Stats> {
    let since = input
        .minutes
        .map(|m| (chrono::Utc::now() - chrono::Duration::minutes(m as i64)).to_rfc3339())
        .unwrap_or_default();
    let model = input.model.clone().unwrap_or_default();
    type Row = (String, bool, i64, i64, Option<f64>, String);
    let rows: Vec<Row> = gw.inner.db.with(|c| {
        let mut s = c.prepare(
            "SELECT outcome, tools, latency_ms, queue_ms, tokens_per_sec, stages FROM inference_log
             WHERE ts >= ?1 AND (?2 = '' OR model = ?2)",
        )?;
        let rows = s.query_map(params![since, model], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        rows.collect()
    })?;
    let mut outcomes = BTreeMap::new();
    let mut interventions = BTreeMap::new();
    let (mut lat, mut queue, mut tps, mut tps_n, mut tools) = (0f64, 0f64, 0f64, 0u64, 0u64);
    for (outcome, t, l, q, rate, stages) in &rows {
        *outcomes.entry(outcome.clone()).or_default() += 1;
        tools += u64::from(*t);
        lat += *l as f64;
        queue += *q as f64;
        if let Some(r) = rate {
            tps += r;
            tps_n += 1;
        }
        for s in serde_json::from_str::<Vec<String>>(stages).unwrap_or_default() {
            *interventions.entry(s).or_default() += 1;
        }
    }
    let n = rows.len().max(1) as f64;
    Ok(Stats {
        requests: rows.len() as u64,
        with_tools: tools,
        outcomes,
        interventions,
        avg_latency_ms: lat / n,
        avg_queue_ms: queue / n,
        avg_tokens_per_sec: (tps_n > 0).then(|| tps / tps_n as f64),
    })
}

fn parse_stages(v: &Value) -> Result<ReliabilityConfig> {
    match v {
        Value::String(s) => ReliabilityConfig::parse(s).map_err(Error::invalid),
        Value::Array(items) => {
            let mut cfg = ReliabilityConfig::off();
            for i in items {
                let s = i
                    .as_str()
                    .ok_or_else(|| Error::invalid("stages must be strings"))?;
                cfg.stages.insert(
                    Stage::parse(s)
                        .ok_or_else(|| Error::invalid(format!("unknown stage '{s}'")))?,
                );
            }
            cfg.max_retries = 2;
            Ok(cfg)
        }
        _ => Err(Error::invalid("stages: 'all', 'off' or a list")),
    }
}

pub fn api_config(
    base_url: &str,
    token: &str,
    client: &str,
    clients_dir: &std::path::Path,
) -> Result<ApiConfig> {
    let mut env = BTreeMap::new();
    let (config, notes) = match client {
        "claude_code" | "claude" => {
            // A separate config dir: a logged-in Claude Code would otherwise send
            // the user's Anthropic credentials to the local base URL.
            env.insert(
                "CLAUDE_CONFIG_DIR".into(),
                clients_dir.join("claude-code").display().to_string(),
            );
            env.insert("ANTHROPIC_BASE_URL".into(), base_url.to_string());
            env.insert("ANTHROPIC_API_KEY".into(), token.to_string());
            env.insert("ANTHROPIC_MODEL".into(), "default".into());
            env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), "default".into());
            env.insert(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            );
            (None, "Run `ancilo claude` – or start Claude Code with these variables. It uses its own configuration directory so your Anthropic login is never sent to the local server.".to_string())
        }
        "codex" => {
            env.insert("ANCILO_TOKEN".into(), token.to_string());
            let cfg = format!(
                "[model_providers.ancilo]\nname = \"Ancilo\"\nbase_url = \"{base_url}/v1\"\nenv_key = \"ANCILO_TOKEN\"\nwire_api = \"responses\"\n\n[profiles.ancilo]\nmodel_provider = \"ancilo\"\nmodel = \"default\"\n"
            );
            (Some(cfg), "Run `ancilo codex` – or add the snippet to ~/.codex/config.toml, export ANCILO_TOKEN and run `codex --profile ancilo`.".to_string())
        }
        "openai" | "generic" => {
            env.insert("OPENAI_BASE_URL".into(), format!("{base_url}/v1"));
            env.insert("OPENAI_API_KEY".into(), token.to_string());
            (None, "Any OpenAI-compatible tool (Aider, Continue, …) works with these settings; use model 'default' or a model id.".to_string())
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown client '{other}' (claude_code, codex, openai)"
            )));
        }
    };
    Ok(ApiConfig {
        base_url: base_url.to_string(),
        env,
        config,
        notes,
    })
}

async fn run_eval(
    gw: &Gateway,
    base_url: &str,
    token: &str,
    dir: &std::path::Path,
    i: EvalInput,
) -> Result<ancilo_eval::Report> {
    let suite = match ancilo_eval::builtin(&i.suite) {
        Some(s) => s,
        None => ancilo_eval::Suite::load(std::path::Path::new(&i.suite))?,
    };
    let model = gw
        .manager()
        .resolve_name(i.model.as_deref().unwrap_or("default"))?;
    let mut reliability = i.reliability.unwrap_or_else(|| "all".into());
    if reliability == "model" {
        reliability = gw.reliability_for(&model).spec();
    }
    ReliabilityConfig::parse(&reliability).map_err(Error::invalid)?;
    let target = ancilo_eval::Target {
        base_url: base_url.to_string(),
        model: model.clone(),
        label: format!("reliability={reliability}"),
        bearer_token: Some(token.to_string()),
        headers: [
            ("x-ancilo-reliability".to_string(), reliability.clone()),
            ("x-ancilo-priority".to_string(), "compare".to_string()),
        ]
        .into_iter()
        .collect(),
    };
    let report = ancilo_eval::run(&suite, &target, i.repeat.unwrap_or(3)).await;
    std::fs::create_dir_all(dir)?;
    let file = dir.join(format!(
        "{}-{}-{}-{}.json",
        suite.name,
        model,
        reliability.replace(',', "+"),
        report.started_at.format("%Y%m%dT%H%M%S")
    ));
    std::fs::write(file, serde_json::to_string_pretty(&report)?)?;
    gw.bus().emit(
        "eval.finished",
        Some(&model),
        serde_json::json!({"suite": report.suite, "label": report.target.label, "success_rate": report.success_rate}),
    );
    Ok(report)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordingInput {
    /// Directory for the recorded scripts; omit to stop recording.
    #[serde(default)]
    pub dir: Option<std::path::PathBuf>,
}

pub fn register(
    registry: &mut Registry,
    gw: Gateway,
    base_url: String,
    token: String,
    evals_dir: std::path::PathBuf,
) {
    let clients_dir = evals_dir
        .parent()
        .map(|h| h.join("clients"))
        .unwrap_or_else(|| evals_dir.join("clients"));
    let g = gw.clone();
    registry.register(
        OpBuilder::new("set_recording")
            .summary("Record real model answers as test scripts (or stop recording)")
            .description("While active, every answer of a local model through the gateway is appended to <dir>/<model>.yaml in the fake-llm script format – so real failure modes become deterministic regression tests.")
            .manage()
            .handler(move |_ctx, i: RecordingInput| {
                let g = g.clone();
                async move {
                    let active = i.dir.is_some();
                    g.set_recording(i.dir);
                    Ok(active)
                }
            }),
    );
    let evals_dir_tune = evals_dir.clone();
    let (g, b, t, d) = (gw.clone(), base_url.clone(), token.clone(), evals_dir);
    registry.register(
        OpBuilder::new("run_eval")
            .summary("Run an eval suite against a model and report success rates")
            .description("Sends every task of the suite several times to the model through the gateway (with the chosen reliability stages) and reports per-task and overall success rates. Reports are saved in the Ancilo home under evals/.")
            .manage()
            .handler(move |_ctx, i: EvalInput| {
                let (g, b, t, d) = (g.clone(), b.clone(), t.clone(), d.clone());
                async move { run_eval(&g, &b, &t, &d, i).await }
            }),
    );
    let g = gw.clone();
    registry.register(
        OpBuilder::new("gateway_stats")
            .summary("Statistics of model calls: outcomes, reliability interventions, latency")
            .handler(move |_ctx, i: StatsInput| {
                let g = g.clone();
                async move { stats(&g, &i) }
            }),
    );
    let g = gw.clone();
    registry.register(
        OpBuilder::new("get_reliability")
            .summary("Show the reliability pipeline stages (global and per model)")
            .handler(move |_ctx, _i: NoInput| {
                let g = g.clone();
                async move {
                    Ok(ReliabilitySettings {
                        global: g.reliability(),
                        models: g.model_reliability(),
                    })
                }
            }),
    );
    let g = gw.clone();
    registry.register(
        OpBuilder::new("set_reliability")
            .summary("Choose the reliability pipeline stages, globally or for one model")
            .manage()
            .handler(move |_ctx, i: SetReliability| {
                let g = g.clone();
                async move {
                    if let Some(m) = &i.model {
                        let id = g.manager().resolve_strict(m)?;
                        if i.stages == "default" {
                            g.set_model_reliability(&id, None)?;
                            return Ok(g.reliability_for(&id));
                        }
                        let mut cfg = parse_stages(&i.stages)?;
                        if let Some(n) = i.max_retries {
                            cfg.max_retries = n;
                        }
                        g.set_model_reliability(&id, Some(cfg.clone()))?;
                        return Ok(cfg);
                    }
                    let mut cfg = parse_stages(&i.stages)?;
                    if let Some(n) = i.max_retries {
                        cfg.max_retries = n;
                    }
                    g.set_reliability(cfg.clone())?;
                    Ok(cfg)
                }
            }),
    );
    let (g, b, t, d) = (gw.clone(), base_url.clone(), token.clone(), evals_dir_tune);
    registry.register(
        OpBuilder::new("tune_reliability")
            .summary("Measure which reliability stages suit a model best and use them for it")
            .description("Runs the tool-calling and no-tool eval suites with each candidate pipeline (all stages, all without the prompt hint, off). Chooses the best tool-calling rate among the candidates that cause no more unwanted tool calls than no pipeline at all, and sets it for this model (unless dry_run). Takes several minutes.")
            .manage()
            .handler(move |_ctx, i: TuneInput| {
                let (g, b, t, d) = (g.clone(), b.clone(), t.clone(), d.clone());
                async move { tune(&g, &b, &t, &d, i).await }
            }),
    );
    registry.register(
        OpBuilder::new("model_api_config")
            .summary("Settings to use the local model from Claude Code, Codex or other tools")
            .description("Returns base URL, environment variables and (for Codex) a config snippet to point a client at Ancilo's OpenAI/Anthropic-compatible model API.")
            .handler(move |_ctx, i: ApiConfigInput| {
                let (b, t, c) = (base_url.clone(), token.clone(), clients_dir.clone());
                async move { api_config(&b, &t, &i.client, &c) }
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(stages: &str, tool: f64, no_tool: f64, eligible: bool) -> TuneCandidate {
        TuneCandidate {
            stages: stages.into(),
            tool_calling: tool,
            no_tool,
            latency_mean_ms: [1000, 1000],
            eligible,
        }
    }

    #[test]
    fn tuning_never_trades_unwanted_tool_calls_for_tool_calling() {
        // Measured on Qwen3-0.6B: the prompt hint costs no-tool answers.
        let small = [
            c("all", 0.80, 0.868, false),
            c("constrained,validate,repair,retry,nudge", 0.80, 0.981, true),
            c("off", 0.75, 0.981, true),
        ];
        assert_eq!(choose(&small), Some(1));
        // Measured on Qwen3.5-4B: all stages win on both sets.
        let medium = [
            c("all", 0.95, 0.962, true),
            c("constrained,validate,repair,retry,nudge", 0.90, 0.925, true),
            c("off", 0.90, 0.925, true),
        ];
        assert_eq!(choose(&medium), Some(0));
        // Ties: the faster one, else the earlier one.
        let tie = [c("all", 0.9, 0.9, true), c("off", 0.9, 0.9, true)];
        assert_eq!(choose(&tie), Some(0));
        let mut slow = c("all", 0.9, 0.9, true);
        slow.latency_mean_ms = [3000, 1000];
        assert_eq!(choose(&[slow, c("off", 0.9, 0.9, true)]), Some(1));
        assert!(within_latency_budget(2000, 1000) && !within_latency_budget(2001, 1000));
    }

    #[test]
    fn stage_specs_round_trip() {
        for s in [
            "off",
            "all",
            "constrained,validate,repair,retry,nudge",
            "prompt,nudge",
        ] {
            assert_eq!(ReliabilityConfig::parse(s).unwrap().spec(), s);
        }
    }
}
