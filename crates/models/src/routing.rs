//! Which model handles a request – one place for the whole precedence:
//!
//! explicit model wish > rule for the task kind > role > default model
//!
//! plus A/B tests: while a test runs for a role, a reproducible share of the
//! requests that are routed through that role goes to model B. Callers report
//! the outcome with [`ModelManager::ab_record`]; a clearly worse B is stopped
//! automatically (guardrail).

use ancilo_core::stats::{self, Difference, Rate};
use ancilo_core::{Error, NoInput, OpBuilder, Registry, Result};
use ancilo_storage::rusqlite::{OptionalExtension, params};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::manager::{ModelManager, ROLE_DEFAULT, ROLE_EMBED};

/// Task kinds a rule can be set for.
pub const KINDS: &[&str] = &["tests", "refactor", "fix", "docs", "summary", "other"];

/// Roles Ancilo knows even while no model holds them.
pub const KNOWN_ROLES: &[&str] = &[
    ROLE_DEFAULT,
    ROLE_EMBED,
    "delegation",
    "coding",
    "assistant",
];

/// Estimates the kind of a task from its text (German and English keywords).
/// Deterministic on purpose: the same task always takes the same route.
pub fn estimate_kind(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["test", "unittest", "spec file", "coverage", "prüf"]) {
        "tests"
    } else if has(&[
        "refactor",
        "rename",
        "extract",
        "umbenenn",
        "extrahier",
        "restructure",
        "umstruktur",
        "clean up",
        "aufräum",
        "simplify",
        "vereinfach",
    ]) {
        "refactor"
    } else if has(&[
        "fix", "bug", "error", "fehler", "broken", "kaputt", "crash", "fails", "failing", "behebe",
        "repair", "repari",
    ]) {
        "fix"
    } else if has(&[
        "docs",
        "documentation",
        "document",
        "readme",
        "docstring",
        "comment",
        "dokumentation",
        "kommentar",
        "changelog",
    ]) {
        "docs"
    } else if has(&[
        "summar",
        "zusammenfass",
        "explain",
        "erklär",
        "describe",
        "beschreib",
        "overview",
        "überblick",
        "what does",
    ]) {
        "summary"
    } else {
        "other"
    }
}

/// Where the outcome of an A/B-routed request comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AbSource {
    Delegation,
    Api,
}

impl AbSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delegation => "delegation",
            Self::Api => "api",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RouteRequest<'a> {
    /// Model id/name, role or alias the caller asked for.
    pub model: Option<&'a str>,
    /// Task kind hint.
    pub kind: Option<&'a str>,
    /// Role to use when nothing more specific applies.
    pub role: &'a str,
    /// Task text – to estimate the kind when no hint is given.
    pub text: Option<&'a str>,
    /// Take part in a running A/B test (the caller reports the outcome).
    pub ab: Option<AbSource>,
    /// For the A/B log (task id, request id).
    pub subject: Option<&'a str>,
}

impl<'a> RouteRequest<'a> {
    /// Plain name resolution (no kinds, no A/B).
    pub fn name(name: &'a str) -> Self {
        Self {
            model: Some(name),
            role: ROLE_DEFAULT,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Explicit,
    Kind,
    Role,
    Default,
    AbTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Arm {
    A,
    B,
}

impl Arm {
    fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

/// Handed to the caller of an A/B-routed request; report the outcome with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AbTicket {
    pub test_id: String,
    pub seq: u64,
    pub arm: Arm,
    /// Shadow mode: this model should also do the task (arm B, no effect on the result).
    pub shadow_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Routed {
    pub model: String,
    pub via: Via,
    /// The task kind (given or estimated), if one was determined.
    pub kind: Option<String>,
    pub kind_estimated: bool,
    pub ab: Option<AbTicket>,
}

/// What happened to an A/B-routed request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AbOutcome {
    /// Objective success (delegations); `None` when not measurable (model API).
    pub success: Option<bool>,
    pub latency_ms: u64,
    pub error: bool,
    /// Reliability interventions (repairs, retries, nudges).
    pub interventions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbTest {
    pub id: String,
    pub role: String,
    pub model_a: String,
    pub model_b: String,
    /// Share of requests for B (0–1).
    pub share: f64,
    pub seed: u64,
    pub min_samples: u64,
    pub guard_min: u64,
    pub shadow: bool,
    /// running | stopped | guardrail_stopped
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArmReport {
    pub arm: Arm,
    pub model: String,
    pub assigned: u64,
    pub completed: u64,
    /// Objective success (only requests where it is measurable).
    pub success: Option<Rate>,
    /// Requests without error (all completed requests).
    pub no_error: Rate,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub interventions_per_request: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbReport {
    pub test: AbTest,
    pub a: ArmReport,
    pub b: ArmReport,
    /// Which metric the verdict is based on: `success` or `no_error`.
    pub metric: String,
    pub verdict: Difference,
    pub p_value: Option<f64>,
    /// One sentence for people.
    pub summary: String,
}

/// Uniform number in [0, 1) from seed and sequence number (SplitMix64).
fn unit(seed: u64, seq: u64) -> f64 {
    let mut z = seed ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Which arm request number `seq` of a test goes to.
pub fn assign_arm(seed: u64, seq: u64, share: f64) -> Arm {
    if unit(seed, seq) < share {
        Arm::B
    } else {
        Arm::A
    }
}

fn test_from_row(
    r: &ancilo_storage::rusqlite::Row<'_>,
) -> ancilo_storage::rusqlite::Result<AbTest> {
    Ok(AbTest {
        id: r.get(0)?,
        role: r.get(1)?,
        model_a: r.get(2)?,
        model_b: r.get(3)?,
        share: r.get(4)?,
        seed: r.get::<_, i64>(5)? as u64,
        min_samples: r.get::<_, i64>(6)? as u64,
        guard_min: r.get::<_, i64>(7)? as u64,
        shadow: r.get::<_, i64>(8)? != 0,
        status: r.get(9)?,
        started_at: r.get(10)?,
        ended_at: r.get(11)?,
        end_reason: r.get(12)?,
    })
}

const TEST_COLS: &str = "id, role, model_a, model_b, share, seed, min_samples, guard_min, shadow, status, started_at, ended_at, end_reason";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbStartInput {
    /// Role under test, e.g. `delegation` or `default`.
    pub role: String,
    /// Challenger model.
    pub b: String,
    /// Incumbent; default: the model the role uses now.
    #[serde(default)]
    pub a: Option<String>,
    /// Share of requests for B: 0–1 or a percentage (e.g. 20).
    #[serde(default)]
    pub share: Option<f64>,
    /// Seed of the assignment (same seed → same assignment sequence).
    #[serde(default)]
    pub seed: Option<u64>,
    /// Samples per arm before any statement (default 30).
    #[serde(default)]
    pub min_samples: Option<u64>,
    /// Samples per arm before the guardrail may stop the test (default 10).
    #[serde(default)]
    pub guard_min: Option<u64>,
    /// B additionally does every delegation in its own worktree, without effect
    /// on the result (costs GPU time).
    #[serde(default)]
    pub shadow: bool,
}

impl ModelManager {
    /// Chooses the model for a request. See the module docs for the order.
    pub fn route(&self, req: &RouteRequest<'_>) -> Result<Routed> {
        let mut role = req.role.to_string();
        if let Some(m) = req.model.map(str::trim).filter(|m| !m.is_empty()) {
            if let Ok(r) = self.find(m) {
                return Ok(Routed {
                    model: r.id,
                    via: Via::Explicit,
                    kind: None,
                    kind_estimated: false,
                    ab: None,
                });
            }
            if KNOWN_ROLES.contains(&m) || self.role_holder(m)?.is_some() {
                role = m.to_string();
            } else if self.config().model_aliases.contains_key(m) {
                let target = self.resolve_plain(m)?;
                return Ok(Routed {
                    model: target,
                    via: Via::Explicit,
                    kind: None,
                    kind_estimated: false,
                    ab: None,
                });
            }
            // Anything else (e.g. `claude-sonnet-…` from a client) → the role.
        }
        let (kind, kind_estimated) = match req.kind.map(|k| k.trim().to_lowercase()) {
            Some(k) if KINDS.contains(&k.as_str()) => (Some(k), false),
            _ => match req.text {
                Some(t) if role != ROLE_EMBED => (Some(estimate_kind(t).to_string()), true),
                _ => (None, false),
            },
        };
        if let Some(k) = &kind {
            let rule: Option<String> = self.db().with(|c| {
                c.query_row(
                    "SELECT model_id FROM routes WHERE kind = ?1",
                    params![k],
                    |r| r.get(0),
                )
                .optional()
            })?;
            if let Some(model) = rule {
                return Ok(Routed {
                    model,
                    via: Via::Kind,
                    kind,
                    kind_estimated,
                    ab: None,
                });
            }
        }
        let (model, via) = match self.role_holder(&role)? {
            Some(m) => (m, Via::Role),
            None => (
                self.role_holder(ROLE_DEFAULT)?.ok_or_else(|| {
                    Error::not_found("no default model yet – add one with `ancilo add <address>`")
                })?,
                Via::Default,
            ),
        };
        let mut routed = Routed {
            model,
            via,
            kind,
            kind_estimated,
            ab: None,
        };
        if let Some(source) = req.ab
            && let Some((ticket, model)) = self.ab_assign(&role, source, req.subject)?
        {
            routed.model = model;
            routed.via = Via::AbTest;
            routed.ab = Some(ticket);
        }
        Ok(routed)
    }

    fn running_test(&self, role: &str) -> Result<Option<AbTest>> {
        self.db().with(|c| {
            c.query_row(
                &format!("SELECT {TEST_COLS} FROM ab_tests WHERE role = ?1 AND status = 'running'"),
                params![role],
                test_from_row,
            )
            .optional()
        })
    }

    fn ab_assign(
        &self,
        role: &str,
        source: AbSource,
        subject: Option<&str>,
    ) -> Result<Option<(AbTicket, String)>> {
        let Some(test) = self.running_test(role)? else {
            return Ok(None);
        };
        if self.record(&test.model_a)?.is_none() || self.record(&test.model_b)?.is_none() {
            self.end_test(&test.id, "stopped", "a model of the test was removed")?;
            return Ok(None);
        }
        let seq: i64 = self.db().with(|c| {
            c.query_row(
                "UPDATE ab_tests SET next_seq = next_seq + 1 WHERE id = ?1 RETURNING next_seq - 1",
                params![test.id],
                |r| r.get(0),
            )
        })?;
        let seq = seq as u64;
        // Shadow mode: the result always comes from A; B works alongside.
        let arm = if test.shadow {
            Arm::A
        } else {
            assign_arm(test.seed, seq, test.share)
        };
        let model = match arm {
            Arm::A => test.model_a.clone(),
            Arm::B => test.model_b.clone(),
        };
        self.log_assignment(&test.id, seq, arm, &model, source, subject)?;
        let ticket = AbTicket {
            test_id: test.id.clone(),
            seq,
            arm,
            shadow_model: (test.shadow && source == AbSource::Delegation)
                .then(|| test.model_b.clone()),
        };
        Ok(Some((ticket, model)))
    }

    fn log_assignment(
        &self,
        test_id: &str,
        seq: u64,
        arm: Arm,
        model: &str,
        source: AbSource,
        subject: Option<&str>,
    ) -> Result<()> {
        self.db().with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO ab_assignments(test_id, seq, arm, model_id, source, subject, ts) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![test_id, seq as i64, arm.as_str(), model, source.as_str(), subject, Utc::now().to_rfc3339()],
            )
            .map(|_| ())
        })?;
        self.bus().emit(
            "ab.assigned",
            Some(test_id),
            json!({"seq": seq, "arm": arm, "model": model, "source": source, "subject": subject}),
        );
        Ok(())
    }

    /// Shadow mode: logs that B also works on request `seq` (as arm B).
    pub fn ab_shadow_assigned(&self, ticket: &AbTicket, subject: Option<&str>) -> Result<AbTicket> {
        let test = self.ab_test(&ticket.test_id)?;
        self.log_assignment(
            &test.id,
            ticket.seq,
            Arm::B,
            &test.model_b,
            AbSource::Delegation,
            subject,
        )?;
        Ok(AbTicket {
            arm: Arm::B,
            shadow_model: None,
            ..ticket.clone()
        })
    }

    /// Reports the outcome of an A/B-routed request; may trigger the guardrail.
    pub fn ab_record(&self, ticket: &AbTicket, outcome: &AbOutcome) -> Result<()> {
        self.db().with(|c| {
            c.execute(
                "UPDATE ab_assignments SET success = ?4, latency_ms = ?5, error = ?6, interventions = ?7
                 WHERE test_id = ?1 AND seq = ?2 AND arm = ?3",
                params![
                    ticket.test_id,
                    ticket.seq as i64,
                    ticket.arm.as_str(),
                    outcome.success,
                    outcome.latency_ms as i64,
                    outcome.error,
                    outcome.interventions
                ],
            )
            .map(|_| ())
        })?;
        let report = self.ab_report(&ticket.test_id)?;
        if report.test.status != "running" {
            return Ok(());
        }
        let (a, b) = match report.metric.as_str() {
            "success" => (
                report.a.success.unwrap_or(Rate::new(0, 0)),
                report.b.success.unwrap_or(Rate::new(0, 0)),
            ),
            _ => (report.a.no_error, report.b.no_error),
        };
        if stats::clearly_worse(&a, &b, report.test.guard_min) {
            let reason = format!(
                "guardrail: B clearly worse ({} {:.0} % [{:.0}–{:.0} %] vs. A {:.0} % [{:.0}–{:.0} %])",
                report.metric,
                b.rate * 100.0,
                b.low * 100.0,
                b.high * 100.0,
                a.rate * 100.0,
                a.low * 100.0,
                a.high * 100.0
            );
            self.end_test(&ticket.test_id, "guardrail_stopped", &reason)?;
            self.bus().emit(
                "ab.guardrail_stopped",
                Some(&ticket.test_id),
                json!({"reason": reason, "a": a, "b": b}),
            );
        }
        Ok(())
    }

    fn end_test(&self, id: &str, status: &str, reason: &str) -> Result<()> {
        let changed = self.db().with(|c| {
            c.execute(
                "UPDATE ab_tests SET status = ?2, ended_at = ?3, end_reason = ?4 WHERE id = ?1 AND status = 'running'",
                params![id, status, Utc::now().to_rfc3339(), reason],
            )
        })?;
        if changed > 0 {
            self.bus().emit(
                "ab.finished",
                Some(id),
                json!({"status": status, "reason": reason}),
            );
        }
        Ok(())
    }

    pub fn ab_start(&self, input: &AbStartInput) -> Result<AbTest> {
        let role = input.role.trim();
        if role.is_empty() {
            return Err(Error::invalid("role is empty"));
        }
        if role == ROLE_EMBED {
            return Err(Error::invalid(
                "A/B tests compare chat models; `embed` is not supported",
            ));
        }
        let b = self.find(&input.b)?.id;
        let a = match &input.a {
            Some(a) => self.find(a)?.id,
            None => self
                .role_holder(role)?
                .or(self.role_holder(ROLE_DEFAULT)?)
                .ok_or_else(|| Error::not_found("no model for this role yet"))?,
        };
        if a == b {
            return Err(Error::invalid(format!("A and B are the same model ({a})")));
        }
        let mut share = input.share.unwrap_or(0.2);
        if share > 1.0 {
            share /= 100.0;
        }
        if !(share > 0.0 && share <= 1.0) {
            return Err(Error::invalid("share must be in (0, 1] or a percentage"));
        }
        if self.running_test(role)?.is_some() {
            return Err(Error::Conflict(format!(
                "an A/B test for role '{role}' is already running – stop it first"
            )));
        }
        let id = format!("ab-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let seed = input.seed.unwrap_or_else(|| rand_seed(&id));
        let min_samples = input.min_samples.unwrap_or(30).max(1);
        let guard_min = input.guard_min.unwrap_or(10).max(1);
        self.db().with(|c| {
            c.execute(
                "INSERT INTO ab_tests(id, role, model_a, model_b, share, seed, min_samples, guard_min, shadow, status, started_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'running', ?10)",
                params![id, role, a, b, share, seed as i64, min_samples as i64, guard_min as i64, input.shadow, Utc::now().to_rfc3339()],
            )
            .map(|_| ())
        })?;
        let test = self.ab_test(&id)?;
        self.bus().emit("ab.started", Some(&id), json!(test));
        Ok(test)
    }

    /// Ends a test by id or role.
    pub fn ab_stop(&self, reference: &str) -> Result<AbTest> {
        let test = self.ab_find(reference)?;
        self.end_test(&test.id, "stopped", "stopped by the user")?;
        self.ab_test(&test.id)
    }

    pub fn ab_test(&self, id: &str) -> Result<AbTest> {
        self.db()
            .with(|c| {
                c.query_row(
                    &format!("SELECT {TEST_COLS} FROM ab_tests WHERE id = ?1"),
                    params![id],
                    test_from_row,
                )
                .optional()
            })?
            .ok_or_else(|| Error::not_found(format!("no A/B test '{id}'")))
    }

    /// A test by id, or the running (else latest) test of a role.
    pub fn ab_find(&self, reference: &str) -> Result<AbTest> {
        if let Ok(t) = self.ab_test(reference) {
            return Ok(t);
        }
        self.db()
            .with(|c| {
                c.query_row(
                    &format!("SELECT {TEST_COLS} FROM ab_tests WHERE role = ?1 ORDER BY status = 'running' DESC, started_at DESC LIMIT 1"),
                    params![reference],
                    test_from_row,
                )
                .optional()
            })?
            .ok_or_else(|| Error::not_found(format!("no A/B test '{reference}'")))
    }

    pub fn ab_list(&self) -> Result<Vec<AbTest>> {
        self.db().with(|c| {
            let mut s = c.prepare(&format!(
                "SELECT {TEST_COLS} FROM ab_tests ORDER BY status = 'running' DESC, started_at DESC"
            ))?;
            let rows = s.query_map([], test_from_row)?;
            rows.collect()
        })
    }

    pub fn ab_report(&self, reference: &str) -> Result<AbReport> {
        let test = self.ab_find(reference)?;
        type Row = (String, Option<bool>, Option<i64>, Option<bool>, Option<i64>);
        let rows: Vec<Row> = self.db().with(|c| {
            let mut s = c.prepare(
                "SELECT arm, success, latency_ms, error, interventions FROM ab_assignments WHERE test_id = ?1",
            )?;
            let rows = s.query_map(params![test.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect()
        })?;
        let arm_report = |arm: Arm, model: &str| {
            let mine: Vec<&Row> = rows.iter().filter(|r| r.0 == arm.as_str()).collect();
            let done: Vec<&&Row> = mine.iter().filter(|r| r.2.is_some()).collect();
            let measured: Vec<bool> = done.iter().filter_map(|r| r.1).collect();
            let lat: Vec<u64> = done.iter().filter_map(|r| r.2).map(|v| v as u64).collect();
            let ok = done.iter().filter(|r| r.3 != Some(true)).count() as u64;
            let interventions: i64 = done.iter().filter_map(|r| r.4).sum();
            ArmReport {
                arm,
                model: model.to_string(),
                assigned: mine.len() as u64,
                completed: done.len() as u64,
                success: (!measured.is_empty()).then(|| {
                    Rate::new(
                        measured.iter().filter(|s| **s).count() as u64,
                        measured.len() as u64,
                    )
                }),
                no_error: Rate::new(ok, done.len() as u64),
                latency_p50_ms: stats::percentile(&lat, 50.0),
                latency_p95_ms: stats::percentile(&lat, 95.0),
                interventions_per_request: (!done.is_empty())
                    .then(|| interventions as f64 / done.len() as f64),
            }
        };
        let a = arm_report(Arm::A, &test.model_a);
        let b = arm_report(Arm::B, &test.model_b);
        let (metric, ra, rb) = match (a.success, b.success) {
            (Some(x), Some(y)) => ("success", x, y),
            _ => ("no_error", a.no_error, b.no_error),
        };
        let (verdict, p_value) = stats::compare_rates(&ra, &rb, test.min_samples, 0.05);
        let pct = |r: &Rate| format!("{:.0} %", r.rate * 100.0);
        let summary = match verdict {
            Difference::InsufficientData => format!(
                "Not enough data yet: {} of {} results per arm needed (A {}, B {}).",
                ra.n.min(rb.n),
                test.min_samples,
                ra.n,
                rb.n
            ),
            Difference::SecondBetter => format!(
                "B ({}) is better: {} {} vs. {} (p = {:.3}).",
                test.model_b,
                metric,
                pct(&rb),
                pct(&ra),
                p_value.unwrap_or(0.0)
            ),
            Difference::SecondWorse => format!(
                "B ({}) is worse: {} {} vs. {} (p = {:.3}).",
                test.model_b,
                metric,
                pct(&rb),
                pct(&ra),
                p_value.unwrap_or(0.0)
            ),
            Difference::NoSignificantDifference => format!(
                "No significant difference: {} {} (B) vs. {} (A).",
                metric,
                pct(&rb),
                pct(&ra)
            ),
        };
        Ok(AbReport {
            test,
            a,
            b,
            metric: metric.into(),
            verdict,
            p_value,
            summary,
        })
    }

    // ---- rules per task kind ---------------------------------------------

    pub fn set_route(&self, kind: &str, model: &str) -> Result<RouteRule> {
        let kind = normalize_kind(kind)?;
        let id = self.find(model)?.id;
        self.db().with(|c| {
            c.execute(
                "INSERT INTO routes(kind, model_id) VALUES(?1, ?2) ON CONFLICT(kind) DO UPDATE SET model_id = excluded.model_id",
                params![kind, id],
            )
            .map(|_| ())
        })?;
        self.bus()
            .emit("route.set", Some(&id), json!({"kind": kind}));
        Ok(RouteRule { kind, model: id })
    }

    pub fn remove_route(&self, kind: &str) -> Result<()> {
        let kind = normalize_kind(kind)?;
        let n = self
            .db()
            .with(|c| c.execute("DELETE FROM routes WHERE kind = ?1", params![kind]))?;
        if n == 0 {
            return Err(Error::not_found(format!("no rule for '{kind}'")));
        }
        self.bus()
            .emit("route.removed", None, json!({"kind": kind}));
        Ok(())
    }

    pub fn routes(&self) -> Result<Vec<RouteRule>> {
        self.db().with(|c| {
            let mut s = c.prepare("SELECT kind, model_id FROM routes ORDER BY kind")?;
            let rows = s.query_map([], |r| {
                Ok(RouteRule {
                    kind: r.get(0)?,
                    model: r.get(1)?,
                })
            })?;
            rows.collect()
        })
    }
}

fn normalize_kind(kind: &str) -> Result<String> {
    let k = kind.trim().to_lowercase();
    if KINDS.contains(&k.as_str()) {
        Ok(k)
    } else {
        Err(Error::invalid(format!(
            "unknown kind '{kind}' – use one of: {}",
            KINDS.join(", ")
        )))
    }
}

fn rand_seed(id: &str) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    id.bytes()
        .fold(nanos, |h, b| h.rotate_left(5) ^ u64::from(b))
        & 0x7FFF_FFFF_FFFF_FFFF
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RouteRule {
    pub kind: String,
    pub model: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteInput {
    /// tests, refactor, fix, docs, summary, other
    pub kind: String,
    pub model: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KindInput {
    pub kind: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbRef {
    /// Test id or role.
    pub test: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainInput {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Default: delegation.
    #[serde(default)]
    pub role: Option<String>,
    /// Task text (to estimate the kind).
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Done {
    pub ok: bool,
}

pub fn register(registry: &mut Registry, manager: ModelManager) {
    let m = manager.clone();
    registry.register(
        OpBuilder::new("set_route")
            .summary(
                "Use a model for one kind of task (tests, refactor, fix, docs, summary, other)",
            )
            .manage()
            .handler(move |_ctx, i: RouteInput| {
                let m = m.clone();
                async move { m.set_route(&i.kind, &i.model) }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("remove_route")
            .summary("Remove the rule for a kind of task")
            .manage()
            .handler(move |_ctx, i: KindInput| {
                let m = m.clone();
                async move { m.remove_route(&i.kind).map(|_| Done { ok: true }) }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("list_routes")
            .summary("Rules per kind of task")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.routes() }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("explain_route")
            .summary("Which model would handle a request, and why")
            .handler(move |_ctx, i: ExplainInput| {
                let m = m.clone();
                async move {
                    let role = i.role.clone().unwrap_or_else(|| "delegation".into());
                    m.route(&RouteRequest {
                        model: i.model.as_deref(),
                        kind: i.kind.as_deref(),
                        role: &role,
                        text: i.task.as_deref(),
                        ab: None,
                        subject: None,
                    })
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("ab_start")
            .summary("Start an A/B test: a share of a role's requests goes to model B")
            .manage()
            .handler(move |_ctx, i: AbStartInput| {
                let m = m.clone();
                async move { m.ab_start(&i) }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("ab_stop")
            .summary("Stop an A/B test (by id or role)")
            .manage()
            .handler(move |_ctx, i: AbRef| {
                let m = m.clone();
                async move { m.ab_stop(&i.test) }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("ab_status")
            .summary("A/B tests, running ones first")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.ab_list() }
            }),
    );
    let m = manager;
    registry.register(
        OpBuilder::new("ab_report")
            .summary("Results of an A/B test with confidence intervals and a verdict")
            .handler(move |_ctx, i: AbRef| {
                let m = m.clone();
                async move { m.ab_report(&i.test) }
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_estimated_from_keywords() {
        assert_eq!(estimate_kind("Write unit tests for src/parser.rs"), "tests");
        assert_eq!(estimate_kind("Schreibe Tests für den Parser"), "tests");
        assert_eq!(estimate_kind("Rename the function foo to bar"), "refactor");
        assert_eq!(estimate_kind("Fix the off-by-one error in range()"), "fix");
        assert_eq!(estimate_kind("Behebe den Fehler beim Login"), "fix");
        assert_eq!(
            estimate_kind("Add a docstring to every public function"),
            "docs"
        );
        assert_eq!(estimate_kind("Summarize what this module does"), "summary");
        assert_eq!(estimate_kind("Add a --verbose flag"), "other");
    }

    // covers: M4-AC-07
    #[test]
    fn assignment_is_reproducible_and_keeps_the_share() {
        let arms: Vec<Arm> = (0..2000).map(|i| assign_arm(42, i, 0.2)).collect();
        let again: Vec<Arm> = (0..2000).map(|i| assign_arm(42, i, 0.2)).collect();
        assert_eq!(arms, again);
        let b = arms.iter().filter(|a| **a == Arm::B).count() as f64 / 2000.0;
        // 99.9 % band for p = 0.2, n = 2000: ±0.029
        assert!((b - 0.2).abs() < 0.03, "share {b}");
        let other: Vec<Arm> = (0..2000).map(|i| assign_arm(7, i, 0.2)).collect();
        assert_ne!(arms, other);
        assert!((0..100).all(|i| assign_arm(1, i, 1.0) == Arm::B));
    }
}
