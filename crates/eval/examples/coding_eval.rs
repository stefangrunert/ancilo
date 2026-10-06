//! Runs a coding-eval suite against a running Ancilo by its home folder –
//! the same checks against any build (the lab compares Ancilo before and
//! after a change, each with a home of its own):
//!
//!   cargo run --release -p ancilo-eval --example coding_eval -- \
//!       <suite.yaml> <ANCILO_HOME> <model> <repeats> <report.json>

use ancilo_eval::coding;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let [_, suite, home, model, repeats, out] = &a[..] else {
        eprintln!("usage: coding_eval <suite.yaml> <ANCILO_HOME> <model> <repeats> <report.json>");
        std::process::exit(2);
    };
    let home = std::path::Path::new(home);
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("daemon.json")).expect("daemon.json"))
            .expect("daemon.json");
    let token = std::fs::read_to_string(home.join("token")).expect("token");
    let text = std::fs::read_to_string(suite).expect("suite");
    let suite: coding::Suite =
        serde_json::from_value(ancilo_eval::yaml_value(&text).expect("yaml")).expect("suite");
    let report = coding::run(
        &suite,
        info["url"].as_str().expect("url"),
        token.trim(),
        model,
        repeats.parse().expect("repeats"),
    )
    .await
    .expect("run");
    std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    for t in &report.tasks {
        let runs: Vec<String> = t
            .runs
            .iter()
            .map(|r| {
                format!(
                    "{}{}s/{}calls/{}reread",
                    if r.passed { "✓" } else { "✗" },
                    r.duration_ms / 1000,
                    r.tool_calls.unwrap_or(0),
                    r.reread.unwrap_or(0)
                )
            })
            .collect();
        println!("{:<28} {}", t.id, runs.join("  "));
        for r in t.runs.iter().filter(|r| !r.passed) {
            println!("    {}", r.failure.as_deref().unwrap_or_default());
        }
    }
    println!("success {:.0}%", report.success_rate * 100.0);
}
