//! Comparison operations – REST, CLI, MCP and the assistant.

use std::time::Duration;

use ancilo_core::{NoInput, OpBuilder, Registry};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{CompareInput, Comparer, SuiteRunInput};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRef {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportInput {
    pub id: String,
    /// Wait up to this many seconds for the comparison to finish.
    #[serde(default)]
    pub wait_s: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateInput {
    pub id: String,
    /// The best label (`A`, `B`, …) or `tie`.
    pub best: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveSuiteInput {
    /// Letters, digits, `-` and `_`.
    pub name: String,
    /// The suite as YAML (same format as the delegation eval).
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaderboardInput {
    /// Only this task kind.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendationsInput {
    /// Include applied and dismissed ones.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendationRef {
    pub id: String,
}

pub fn register(registry: &mut Registry, comparer: Comparer) {
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("compare_models")
            .summary("Run the same task on several models, each in its own worktree from the same commit; returns a comparison id")
            .manage()
            .handler(move |_ctx, i: CompareInput| {
                let c = c.clone();
                async move { c.compare(i).await }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("comparison_status")
            .summary("Progress of a comparison")
            .handler(move |_ctx, i: ComparisonRef| {
                let c = c.clone();
                async move {
                    c.report(&i.id).map(|mut r| {
                        r.runs.clear();
                        r
                    })
                }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("comparison_report")
            .summary("Report of a comparison: success, duration, load time, tokens/s, tokens, steps, interventions, diff size – per model")
            .handler(move |_ctx, i: ReportInput| {
                let c = c.clone();
                async move { c.wait_report(&i.id, i.wait_s.map(Duration::from_secs)).await }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("list_comparisons")
            .summary("Recent comparisons and suite runs")
            .handler(move |_ctx, i: ListInput| {
                let c = c.clone();
                async move { c.list(i.limit.unwrap_or(20)) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("cancel_comparison")
            .summary("Stop a running comparison")
            .manage()
            .handler(move |_ctx, i: ComparisonRef| {
                let c = c.clone();
                async move { c.cancel(&i.id) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("rate_comparison")
            .summary("Rate a finished comparison (reveals the models of a blind one)")
            .manage()
            .handler(move |_ctx, i: RateInput| {
                let c = c.clone();
                async move { c.rate(&i.id, &i.best, i.note) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("save_suite")
            .summary("Save a task collection (eval format) as a personal benchmark")
            .manage()
            .handler(move |_ctx, i: SaveSuiteInput| {
                let c = c.clone();
                async move { c.save_suite(&i.name, &i.content) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("list_suites")
            .summary("Built-in and saved suites")
            .handler(move |_ctx, _i: NoInput| {
                let c = c.clone();
                async move { c.suites() }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("run_suite")
            .summary("Run a suite on several models; returns a comparison id")
            .manage()
            .handler(move |_ctx, i: SuiteRunInput| {
                let c = c.clone();
                async move { c.run_suite(i).await }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("leaderboard")
            .summary("Models ranked per task kind on this machine (with Markdown export)")
            .handler(move |_ctx, i: LeaderboardInput| {
                let c = c.clone();
                async move { c.leaderboard(i.kind.as_deref()) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("recommendations")
            .summary("Suggested model assignments backed by enough data")
            .handler(move |_ctx, i: RecommendationsInput| {
                let c = c.clone();
                async move { c.recommendations(i.all) }
            }),
    );
    let c = comparer.clone();
    registry.register(
        OpBuilder::new("apply_recommendation")
            .summary("Carry out a recommendation (changes a model assignment)")
            .consequential()
            .handler(move |_ctx, i: RecommendationRef| {
                let c = c.clone();
                async move { c.apply_recommendation(&i.id) }
            }),
    );
    let c = comparer;
    registry.register(
        OpBuilder::new("dismiss_recommendation")
            .summary("Dismiss a recommendation; it is not suggested again")
            .manage()
            .handler(move |_ctx, i: RecommendationRef| {
                let c = c.clone();
                async move { c.dismiss_recommendation(&i.id) }
            }),
    );
}
