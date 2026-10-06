//! FPL-02: what survives when long tool output has to get shorter – the
//! cases of `evals/fpl02/` through three variants at the same budget:
//!
//! - `bestand`: Ancilo before (a command's output clipped to its start and
//!   end at 20 000 characters, every later cut keeps the start),
//! - `port`: Atomic Agent's compressor as ported (`compress`), at the
//!   case's budget, on the same clipped output,
//! - `erweitert`: Ancilo now (`results::shorten` – by kind, with status and
//!   the id of the whole result – at the tool and at the cut).
//!
//! `FPL02_CASES=<file>` runs another set (the held-back one),
//! `FPL02_REPORT=<file>` writes the results as JSON.

use std::path::PathBuf;

use ancilo_agent::compress::{self, Options, Overflow};
use ancilo_agent::results::{self, Meta};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    id: String,
    #[serde(default)]
    group: String,
    tool: String,
    is_error: bool,
    output: String,
    budget_chars: usize,
    must_keep: Vec<String>,
    #[serde(default)]
    must_not_keep: Vec<String>,
    #[serde(default)]
    starts_with: Option<String>,
}

/// The clip of a command's output before (start and end, half each).
const TOOL_LIMIT: usize = 20_000;
fn old_clip(text: &str) -> String {
    if text.len() <= TOOL_LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(TOOL_LIMIT / 2).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(TOOL_LIMIT / 2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n… ({} characters omitted) …\n{tail}",
        text.len() - TOOL_LIMIT
    )
}

/// The cut before: the start.
fn old_cut(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    format!(
        "{}… (shortened)",
        text.chars().take(budget).collect::<String>()
    )
}

fn variant(name: &str, c: &Case) -> String {
    let at_tool = c.tool == "bash";
    match name {
        "bestand" => {
            let t = if at_tool {
                old_clip(&c.output)
            } else {
                c.output.clone()
            };
            old_cut(&t, c.budget_chars)
        }
        "port" => {
            let t = if at_tool {
                old_clip(&c.output)
            } else {
                c.output.clone()
            };
            if t.chars().count() <= c.budget_chars {
                return t;
            }
            compress::compress(
                &t,
                c.is_error,
                Options {
                    max_summary_len: c.budget_chars,
                    max_tail_lines: 12,
                    overflow: Overflow::Head,
                },
            )
            .summary
        }
        "erweitert" => {
            // As the agent does it: cut at the tool (the whole kept), then cut
            // again later – the record says what the whole was.
            let meta = Meta {
                tool: c.tool.clone(),
                error: c.is_error,
                id: Some("r1".into()),
                cut: None,
            };
            if !at_tool || c.output.chars().count() <= TOOL_LIMIT {
                return results::shorten(&meta, &c.output, c.budget_chars);
            }
            let t = results::shorten(&meta, &c.output, TOOL_LIMIT);
            let recorded = Meta {
                cut: Some(results::cut_of(&c.output)),
                ..meta
            };
            results::shorten(&recorded, &t, c.budget_chars)
        }
        // The cutter alone, on the very input the others get (the old clip
        // at the tool) – to tell its part from the tool's (review 1, 18).
        "erweitert@clip" => {
            let t = if at_tool {
                old_clip(&c.output)
            } else {
                c.output.clone()
            };
            let meta = Meta {
                tool: c.tool.clone(),
                error: c.is_error,
                id: Some("r1".into()),
                cut: None,
            };
            results::shorten(&meta, &t, c.budget_chars)
        }
        _ => unreachable!(),
    }
}

/// What a variant's text misses of a case (empty: passed).
fn misses(c: &Case, text: &str) -> Vec<String> {
    let mut m: Vec<String> = c
        .must_keep
        .iter()
        .filter(|k| !text.contains(k.as_str()))
        .map(|k| format!("lost {k:?}"))
        .collect();
    m.extend(
        c.must_not_keep
            .iter()
            .filter(|k| text.contains(k.as_str()))
            .map(|k| format!("kept {k:?}")),
    );
    // What the text starts with: after Ancilo's own label, unless the case
    // asks for the label itself.
    let content = match text.split_once('\n') {
        Some((first, rest))
            if first.starts_with('[')
                && first.contains("shortened from")
                && !c.starts_with.as_deref().is_some_and(|s| s.starts_with('[')) =>
        {
            rest
        }
        _ => text,
    };
    if let Some(s) = &c.starts_with
        && !content.starts_with(s.as_str())
    {
        m.push(format!("does not start with {s:?}"));
    }
    // The budget (the old cut adds its marker on top).
    if text.chars().count() > c.budget_chars + 20 {
        m.push(format!(
            "{} chars over a budget of {}",
            text.chars().count(),
            c.budget_chars
        ));
    }
    m
}

fn cases() -> (PathBuf, Vec<Case>) {
    let path = std::env::var("FPL02_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/fpl02/dev-cases.json")
        });
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (path, serde_json::from_str(&text).expect("cases"))
}

// covers: FPL-02 (deterministic information retention)
#[test]
fn what_survives_the_cut_bestand_port_erweitert() {
    let (path, cases) = cases();
    let names = ["bestand", "port", "erweitert", "erweitert@clip"];
    let mut rows = Vec::new();
    let mut passed = [0usize; 4];
    println!("FPL-02 · {} · {} cases", path.display(), cases.len());
    println!(
        "{:<10} {:<16} {:>6}  {:<9} {:<9} {:<9} {:<9}",
        "case", "group", "budget", names[0], names[1], names[2], names[3]
    );
    for c in &cases {
        let mut row = json!({"id": c.id, "group": c.group, "budget": c.budget_chars});
        let mut marks = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let text = variant(n, c);
            let m = misses(c, &text);
            if m.is_empty() {
                passed[i] += 1;
            }
            marks.push(if m.is_empty() {
                "ok".to_string()
            } else {
                format!("✗{}", m.len())
            });
            row[n] = json!({"passed": m.is_empty(), "misses": m, "chars": text.chars().count(), "text": text});
        }
        println!(
            "{:<10} {:<16} {:>6}  {:<9} {:<9} {:<9} {:<9}",
            c.id, c.group, c.budget_chars, marks[0], marks[1], marks[2], marks[3]
        );
        rows.push(row);
    }
    println!(
        "passed:                            {:<9} {:<9} {:<9} {:<9}",
        passed[0], passed[1], passed[2], passed[3]
    );
    if let Ok(out) = std::env::var("FPL02_REPORT") {
        let report = json!({"cases": path.display().to_string(), "passed": {"bestand": passed[0], "port": passed[1], "erweitert": passed[2], "erweitert@clip": passed[3]}, "rows": rows});
        std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    // The development set is the bar for Ancilo now: every case.
    if std::env::var("FPL02_CASES").is_err() {
        let failed: Vec<&Value> = rows
            .iter()
            .filter(|r| r["erweitert"]["passed"] != true)
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }
}

/// The port behaves like the original: the summaries Atomic Agent's own
/// TypeScript produced for the same inputs (`evals/fpl02/original.json`,
/// made by `evals/fpl02/run-original.mjs`) – where they differ, only by the
/// documented differences.
#[test]
fn the_port_matches_the_original() {
    let path = std::env::var("FPL02_ORIGINAL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/fpl02/original.json")
        });
    let Ok(text) = std::fs::read_to_string(&path) else {
        return; // made with node and the source checkout
    };
    let runs: Vec<Value> = serde_json::from_str(&text).unwrap();
    let (_, cases) = cases();
    let output = |id: &str| cases.iter().find(|c| c.id == id).map(|c| c.output.clone());
    let mut differ = Vec::new();
    for r in &runs {
        let o = &r["options"];
        let opts = Options {
            max_summary_len: o["maxSummaryLength"].as_u64().unwrap() as usize,
            max_tail_lines: o["maxTailLines"]
                .as_u64()
                .map_or(usize::MAX, |n| n as usize),
            overflow: if o["overflow"] == "tail" {
                Overflow::Tail
            } else {
                Overflow::Head
            },
        };
        let Some(input) = output(r["case"].as_str().unwrap()) else {
            continue;
        };
        let ours = compress::compress(&input, r["status"] == "error", opts);
        if ours.summary != r["summary"].as_str().unwrap() || ours.truncated != r["truncated"] {
            differ.push(r["id"].as_str().unwrap().to_string());
        }
    }
    // Only where JavaScript counts UTF-16 units (astral characters).
    let allowed: Vec<String> = runs
        .iter()
        .filter(|r| r["astral"] == true)
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    let unexpected: Vec<&String> = differ.iter().filter(|d| !allowed.contains(d)).collect();
    assert!(
        unexpected.is_empty(),
        "differs from the original: {unexpected:?}"
    );
    println!(
        "{} runs, {} differ (astral only: {:?})",
        runs.len(),
        differ.len(),
        differ
    );
}
