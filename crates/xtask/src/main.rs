//! `cargo run -p xtask -- <command>`
//!
//! - `trace`: every acceptance criterion of an active milestone must be
//!   referenced by at least one test (`covers: M1-AC-03`). Unknown IDs in tests
//!   are errors too.
//! - `status [--json]`: progress towards the complete application – the state
//!   of every criterion, using the last test run (`target/nextest/ci/junit.xml`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Criterion {
    id: String,
    intent: String,
    blocked: bool,
    dropped: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Milestone {
    id: String,
    title: String,
    status: String,
    criteria: Vec<Criterion>,
}

impl Milestone {
    /// Criteria of active milestones must be covered by tests. The status may
    /// carry a remark ("In Arbeit (offen: …)") – only the leading word counts.
    fn active(&self) -> bool {
        ["Verfeinert", "In Arbeit", "Fertig"]
            .iter()
            .any(|s| self.status.starts_with(s))
    }
}

/// A test that declares which criteria it covers.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct Coverage {
    criterion: String,
    file: String,
    krate: String,
    test_fn: Option<String>,
}

fn parse_spec(path: &Path) -> Result<Milestone> {
    let text = std::fs::read_to_string(path)?;
    let id = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('-').next())
        .context("spec file name must start with the milestone id")?
        .to_string();
    let title = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches('#')
        .trim()
        .to_string();
    let status = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("- **Status:**"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "Entwurf".into());
    let prefix = format!("{id}-AC-");
    let mut criteria = Vec::new();
    for line in text.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 4 {
            continue;
        }
        let raw_id = cells[1];
        let dropped = raw_id.starts_with("~~");
        let cid = raw_id.trim_matches('~').to_string();
        if !cid.starts_with(&prefix) {
            continue;
        }
        let rest = cells[2..].join(" ");
        criteria.push(Criterion {
            id: cid,
            intent: cells[2].trim_matches('~').to_string(),
            blocked: rest.to_lowercase().contains("*blockiert*"),
            dropped,
        });
    }
    Ok(Milestone {
        id,
        title,
        status,
        criteria,
    })
}

fn load_specs(root: &Path) -> Result<Vec<Milestone>> {
    let dir = root.join(".devnotes/specs");
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with('M') && name.ends_with(".md") {
            specs.push(parse_spec(&path)?);
        }
    }
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(specs)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if ["rs", "ts", "tsx", "yaml", "yml", "py"]
            .iter()
            .any(|ext| name.ends_with(&format!(".{ext}")))
        {
            out.push(p);
        }
    }
}

fn crate_of(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    match (parts.next().as_deref(), parts.next()) {
        (Some("crates"), Some(dir)) => {
            if dir == "cli" {
                "ancilo".into()
            } else if dir == "xtask" {
                "xtask".into()
            } else {
                format!("ancilo-{dir}")
            }
        }
        (Some(top), _) => top.to_string(),
        _ => String::new(),
    }
}

fn scan_coverage(root: &Path) -> Result<Vec<Coverage>> {
    let mut files = Vec::new();
    for dir in ["crates", "app", "evals", "tests"] {
        walk(&root.join(dir), &mut files);
    }
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(pos) = line.find("covers:") else {
                continue;
            };
            // The marker must be a comment line of its own (not a string literal).
            let before = line[..pos].trim();
            if !(before == "//" || before == "#") {
                continue;
            }
            let ids: Vec<String> = line[pos + 7..]
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| s.contains("-AC-"))
                .map(str::to_string)
                .collect();
            let test_fn = lines[i + 1..].iter().take(12).find_map(|l| {
                let l = l.trim_start();
                let l = l.strip_prefix("pub ").unwrap_or(l);
                let l = l.strip_prefix("async ").unwrap_or(l);
                l.strip_prefix("fn ")
                    .map(|r| r.split('(').next().unwrap_or_default().trim().to_string())
                    .or_else(|| {
                        // TypeScript: test("name", …)
                        l.strip_prefix("test(")
                            .and_then(|r| r.split(['"', '\'']).nth(1))
                            .map(str::to_string)
                    })
            });
            for id in ids {
                out.push(Coverage {
                    criterion: id,
                    file: file
                        .strip_prefix(root)
                        .unwrap_or(&file)
                        .display()
                        .to_string(),
                    krate: crate_of(root, &file),
                    test_fn: test_fn.clone(),
                });
            }
        }
    }
    Ok(out)
}

struct TraceReport {
    missing: Vec<String>,
    unknown: Vec<Coverage>,
}

fn trace(specs: &[Milestone], coverage: &[Coverage]) -> TraceReport {
    let known: BTreeSet<&str> = specs
        .iter()
        .flat_map(|m| m.criteria.iter().map(|c| c.id.as_str()))
        .collect();
    let covered: BTreeSet<&str> = coverage.iter().map(|c| c.criterion.as_str()).collect();
    let missing = specs
        .iter()
        .filter(|m| m.active())
        .flat_map(|m| m.criteria.iter())
        .filter(|c| !c.dropped && !c.blocked && !covered.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect();
    let unknown = coverage
        .iter()
        .filter(|c| !known.contains(c.criterion.as_str()))
        .cloned()
        .collect();
    TraceReport { missing, unknown }
}

/// Test outcomes from nextest's JUnit report: (crate, test fn) → passed.
fn junit_results(root: &Path) -> BTreeMap<(String, String), bool> {
    let mut results = BTreeMap::new();
    let Ok(xml) = std::fs::read_to_string(root.join("target/nextest/ci/junit.xml")) else {
        return results;
    };
    // Minimal parsing: <testcase name="mod::test_fn" classname="crate::bin" …> [<failure|error>]
    let mut rest = xml.as_str();
    while let Some(start) = rest.find("<testcase ") {
        rest = &rest[start..];
        let end_tag = rest.find('>').unwrap_or(rest.len());
        let head = &rest[..end_tag];
        let attr = |name: &str| {
            head.find(&format!("{name}=\""))
                .map(|i| &head[i + name.len() + 2..])
                .and_then(|s| s.split('"').next())
                .unwrap_or_default()
                .to_string()
        };
        let name = attr("name");
        let class = attr("classname");
        let self_closing = head.ends_with('/');
        let body_end = if self_closing {
            end_tag
        } else {
            rest.find("</testcase>").unwrap_or(end_tag)
        };
        let body = &rest[..body_end];
        let failed = body.contains("<failure") || body.contains("<error");
        let krate = class.split("::").next().unwrap_or_default().to_string();
        let func = name.rsplit("::").next().unwrap_or_default().to_string();
        results.insert((krate, func), !failed);
        rest = &rest[end_tag.max(1)..];
    }
    results
}

#[derive(Serialize)]
struct CriterionState {
    id: String,
    intent: String,
    state: &'static str,
}

fn status(root: &Path, json: bool) -> Result<()> {
    let specs = load_specs(root)?;
    let coverage = scan_coverage(root)?;
    let results = junit_results(root);
    let mut out = Vec::new();
    let (mut total, mut green) = (0, 0);
    for m in &specs {
        let mut states = Vec::new();
        for c in m.criteria.iter().filter(|c| !c.dropped) {
            let tests: Vec<&Coverage> = coverage.iter().filter(|cv| cv.criterion == c.id).collect();
            let outcomes: Vec<Option<bool>> = tests
                .iter()
                .map(|t| {
                    t.test_fn
                        .as_ref()
                        .and_then(|f| results.get(&(t.krate.clone(), f.clone())).copied())
                })
                .collect();
            let state = if c.blocked {
                "blockiert"
            } else if tests.is_empty() {
                "ohne Test"
            } else if outcomes.contains(&Some(false)) {
                "rot"
            } else if outcomes.iter().any(|o| o.is_some()) {
                "grün"
            } else {
                "nicht gelaufen"
            };
            total += 1;
            if state == "grün" {
                green += 1;
            }
            states.push(CriterionState {
                id: c.id.clone(),
                intent: c.intent.clone(),
                state,
            });
        }
        out.push((m, states));
    }
    if json {
        let v: Vec<_> = out
            .iter()
            .map(|(m, s)| serde_json::json!({"milestone": m.id, "title": m.title, "status": m.status, "criteria": s}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"green": green, "total": total, "milestones": v})
            )?
        );
        return Ok(());
    }
    println!("Fortschritt zur vollständigen Anwendung: {green}/{total} Kriterien grün\n");
    for (m, states) in &out {
        let g = states.iter().filter(|s| s.state == "grün").count();
        println!("{} [{}] {}/{}", m.title, m.status, g, states.len());
        for s in states {
            let mark = match s.state {
                "grün" => "✓",
                "rot" => "✗",
                "blockiert" => "⏸",
                "nicht gelaufen" => "·",
                _ => " ",
            };
            println!("  {mark} {:<10} {:<15} {}", s.id, s.state, s.intent);
        }
        println!();
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or(manifest)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    match args.first().map(String::as_str) {
        Some("trace") => {
            // The specs are project notes kept outside the public repository.
            if !root.join(".devnotes/specs").is_dir() {
                println!(
                    "trace skipped: no specs here (project notes are not part of this repository)"
                );
                return Ok(());
            }
            let specs = load_specs(&root)?;
            let coverage = scan_coverage(&root)?;
            let report = trace(&specs, &coverage);
            for u in &report.unknown {
                eprintln!("unknown criterion {} referenced in {}", u.criterion, u.file);
            }
            for m in &report.missing {
                eprintln!("criterion {m} of an active milestone has no test");
            }
            if !report.missing.is_empty() || !report.unknown.is_empty() {
                bail!(
                    "trace failed: {} uncovered, {} unknown",
                    report.missing.len(),
                    report.unknown.len()
                );
            }
            let active: usize = specs
                .iter()
                .filter(|m| m.active())
                .map(|m| m.criteria.len())
                .sum();
            println!(
                "trace ok: {} criteria in active milestones, all covered by tests",
                active
            );
            Ok(())
        }
        Some("status") => status(&root, args.iter().any(|a| a == "--json")),
        Some("links") => {
            let broken = check_links(&root)?;
            for b in &broken {
                eprintln!("{b}");
            }
            if !broken.is_empty() {
                bail!("{} broken links", broken.len());
            }
            println!("links ok");
            Ok(())
        }
        Some("release-gate") => {
            // xtask release-gate <commit> [--accept <file>] <junit.xml>…
            // (each junit.xml with a COMMIT file beside it). `--accept`: a
            // pre-release's known gaps – one criterion or milestone per line,
            // with the reason; everything else still blocks.
            let commit = args
                .get(1)
                .context("usage: xtask release-gate <commit> [--accept <file>] <junit.xml>…")?;
            let mut rest = &args[2..];
            let mut accepted = Vec::new();
            if rest.first().map(String::as_str) == Some("--accept") {
                let file = rest.get(1).context("--accept needs a file")?;
                accepted = parse_accepted(&std::fs::read_to_string(file)?)?;
                rest = &rest[2..];
            }
            let files: Vec<PathBuf> = rest.iter().map(PathBuf::from).collect();
            let problems = release_gate(&root, commit, &files)?;
            let (open, known) = split_accepted(problems, &accepted);
            for p in &known {
                println!("accepted (pre-release): {p}");
            }
            for p in &open {
                eprintln!("{p}");
            }
            if !open.is_empty() {
                bail!("release blocked: {} problems", open.len());
            }
            if known.is_empty() {
                println!("release gate ok: every criterion of every milestone passed on {commit}");
            } else {
                println!(
                    "release gate ok for a pre-release: {} known gaps, everything else passed on {commit}",
                    known.len()
                );
            }
            Ok(())
        }
        Some("eval-report") => {
            // xtask eval-report <evals-dir> "<hardware>" → Markdown for docs/evals.md
            let dir = args
                .get(1)
                .context("usage: xtask eval-report <evals-dir> <hardware>")?;
            let hardware = args.get(2).map(String::as_str).unwrap_or("unknown");
            print!("{}", eval_report(Path::new(dir), hardware)?);
            Ok(())
        }
        Some("notices") => {
            print!("{}", notices(&root)?);
            Ok(())
        }
        _ => bail!("usage: xtask <trace|status [--json]|links|notices>"),
    }
}

// ---- release gate (M9-AC-07) ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    Passed,
    Failed,
    Skipped,
}

/// One test case of a JUnit report: (class/file, test name, outcome).
fn junit_cases(xml: &str) -> Vec<(String, String, Outcome)> {
    let unescape = |s: &str| {
        s.replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    };
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<testcase ") {
        rest = &rest[start..];
        let end_tag = rest.find('>').unwrap_or(rest.len());
        let head = &rest[..end_tag];
        let attr = |name: &str| {
            head.find(&format!(" {name}=\""))
                .map(|i| &head[i + name.len() + 3..])
                .and_then(|s| s.split('"').next())
                .map(unescape)
                .unwrap_or_default()
        };
        let body_end = if head.ends_with('/') {
            end_tag
        } else {
            rest.find("</testcase>").unwrap_or(end_tag)
        };
        let body = &rest[..body_end];
        let outcome = if body.contains("<failure") || body.contains("<error") {
            Outcome::Failed
        } else if body.contains("<skipped") {
            Outcome::Skipped
        } else {
            Outcome::Passed
        };
        out.push((attr("classname"), attr("name"), outcome));
        rest = &rest[end_tag.max(1)..];
    }
    out
}

/// Whether a test case belongs to a coverage marker: Rust tests by crate and
/// function, app tests (Playwright, Vitest) by file and title.
fn case_matches(cov: &Coverage, class: &str, name: &str) -> bool {
    let file_name = Path::new(&cov.file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if cov.file.ends_with(".rs") {
        let krate = class.split("::").next().unwrap_or_default();
        return krate == cov.krate
            && cov
                .test_fn
                .as_ref()
                .is_some_and(|f| name.rsplit("::").next() == Some(f.as_str()));
    }
    let same_file = class.ends_with(&file_name) || name.contains(&file_name);
    same_file
        && match &cov.test_fn {
            Some(title) => name.ends_with(title.as_str()) || name.contains(&format!("› {title}")),
            // A marker above a whole suite: every test of the file counts.
            None => true,
        }
}

/// Everything that stops a release: blocked or draft criteria, criteria
/// without tests, tests without a result, failed or skipped tests, results
/// from another commit.
/// The known gaps of a pre-release: `<criterion or milestone>  <reason>` per
/// line (`#` starts a comment). Every gap needs its reason.
fn parse_accepted(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (id, reason) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if reason.trim().is_empty() {
            bail!("accepted gap {id} has no reason");
        }
        out.push((id.to_string(), reason.trim().to_string()));
    }
    Ok(out)
}

/// Splits the gate's problems into those still open and the accepted ones.
fn split_accepted(
    problems: Vec<String>,
    accepted: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let mut open = Vec::new();
    let mut known = Vec::new();
    for p in problems {
        let id = p.split(':').next().unwrap_or_default();
        match accepted.iter().find(|(a, _)| a == id) {
            Some((_, reason)) => known.push(format!("{p} – {reason}")),
            None => open.push(p),
        }
    }
    (open, known)
}

fn release_gate(root: &Path, commit: &str, junit: &[PathBuf]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let mut cases = Vec::new();
    for f in junit {
        let stamp = f.with_file_name("COMMIT");
        match std::fs::read_to_string(&stamp) {
            Ok(c) if c.trim() == commit => {}
            Ok(c) => problems.push(format!(
                "{}: results of {} , not {commit}",
                f.display(),
                c.trim()
            )),
            Err(_) => problems.push(format!(
                "{}: no COMMIT file – cannot tell which commit was tested",
                f.display()
            )),
        }
        let xml = std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
        cases.extend(junit_cases(&xml));
    }
    let specs = load_specs(root)?;
    let coverage = scan_coverage(root)?;
    for m in &specs {
        if !m.active() {
            problems.push(format!(
                "{}: milestone is still a draft ({})",
                m.id, m.status
            ));
        }
        for c in m.criteria.iter().filter(|c| !c.dropped) {
            if c.blocked {
                problems.push(format!("{}: blocked", c.id));
                continue;
            }
            let tests: Vec<&Coverage> = coverage.iter().filter(|cv| cv.criterion == c.id).collect();
            if tests.is_empty() {
                problems.push(format!("{}: no test", c.id));
            }
            for t in tests {
                let label = t.test_fn.clone().unwrap_or_else(|| t.file.clone());
                let results: Vec<Outcome> = cases
                    .iter()
                    .filter(|(class, name, _)| case_matches(t, class, name))
                    .map(|(_, _, o)| *o)
                    .collect();
                if results.is_empty() {
                    problems.push(format!("{}: {label} ({}) has no result", c.id, t.file));
                } else if results.contains(&Outcome::Failed) {
                    problems.push(format!("{}: {label} failed", c.id));
                } else if results.contains(&Outcome::Skipped) {
                    problems.push(format!("{}: {label} was skipped", c.id));
                }
            }
        }
    }
    problems.sort();
    problems.dedup();
    Ok(problems)
}

// ---- published eval results (M9) ----------------------------------------------

/// The latest report per (suite, model, pipeline) as Markdown, with method.
fn eval_report(dir: &Path, hardware: &str) -> Result<String> {
    let mut latest: BTreeMap<(String, String, String), serde_json::Value> = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for f in files {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&f)?) else {
            continue;
        };
        let suite = v["suite"].as_str().unwrap_or_default().to_string();
        // Eval-Set I reports name a target; delegation and coding reports a model.
        let model = v["target"]["model"]
            .as_str()
            .or(v["model"].as_str())
            .unwrap_or_default()
            .to_string();
        // Eval-Set I: the reliability pipeline; delegation: with or without search.
        let pipeline = match (v["target"]["label"].as_str(), v["search"].as_bool()) {
            (Some(label), _) => label.trim_start_matches("reliability=").to_string(),
            (None, Some(true)) => "with search".into(),
            (None, Some(false)) => "without search".into(),
            (None, None) => String::new(),
        };
        if suite.is_empty() || model.is_empty() {
            continue;
        }
        let key = (suite, model, pipeline);
        let newer = latest
            .get(&key)
            .is_none_or(|old| old["started_at"].as_str() < v["started_at"].as_str());
        if newer {
            latest.insert(key, v);
        }
    }
    let pct = |v: &serde_json::Value| {
        v["success_rate"]
            .as_f64()
            .map_or("–".into(), |r| format!("{:.0} %", r * 100.0))
    };
    let runs = |v: &serde_json::Value| {
        let tasks = v["tasks"].as_array().map_or(0, Vec::len) as u64;
        tasks * v["repeats"].as_u64().unwrap_or(1)
    };
    let mut s = format!(
        "# Eval results\n\nHow well local models do Ancilo's jobs – measured with the eval suites in this repository on one machine ({hardware}). Generated by `cargo run -p xtask -- eval-report` from the reports in Ancilo's `evals/` directory; the newest report per suite, model and pipeline.\n\n## Method\n\n- **tool-calling / no-tool** (Eval-Set I, `evals/tool-calling*.yaml`, `evals/no-tool*.yaml`): single requests through Ancilo's model API; a task passes when the model calls the right tool with the right arguments (or, for no-tool, answers without calling one). *Holdout* sets were not used for tuning. The *Setup* column is the reliability pipeline (`off` = the model alone, see [Model API](model-api.md)) or whether the agent had the project search.\n- **delegation** (Eval-Set II, `evals/delegation.yaml`): real tasks delegated through `delegate` in fresh git repositories, judged by checks on the result (files, commands that must succeed, files that must stay unchanged).\n- **delegation-search** (`evals/delegation-search.yaml`): bugs described in the user's words, hidden among 80 unrelated modules – shows whether the project search helps.\n- **coding** (`evals/coding.yaml`): tasks through the coding sessions of the app, some over several turns; changes applied with `apply_changes`, then checked; the project must stay untouched until then.\n- Runs are few (see *Runs*): read differences of a few percent as noise. Reproduce with `ancilo eval <suite> --model <model>`.\n\n"
    );
    let mut suite_now = String::new();
    for ((suite, model, pipeline), v) in &latest {
        if *suite != suite_now {
            s.push_str(&format!("## {suite}\n\n| Model | Setup | Success | Runs | Latency p50 |\n|---|---|---:|---:|---:|\n"));
            suite_now = suite.clone();
        }
        let latency = v["latency_p50_ms"]
            .as_u64()
            .map_or("–".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0));
        s.push_str(&format!(
            "| {model} | {} | {} | {} | {latency} |\n",
            if pipeline.is_empty() {
                "–"
            } else {
                pipeline.as_str()
            },
            pct(v),
            runs(v)
        ));
        if latest.keys().rfind(|k| k.0 == *suite)
            == Some(&(suite.clone(), model.clone(), pipeline.clone()))
        {
            s.push('\n');
        }
    }
    Ok(s)
}

// ---- third-party notices (M9) ------------------------------------------------

struct Component {
    name: String,
    version: String,
    license: String,
    texts: Vec<String>,
}

/// License files of a package directory (`LICENSE`, `LICENSE-MIT`, `COPYING`, …).
fn license_texts(dir: &Path) -> Vec<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_uppercase();
            p.is_file()
                && (n.starts_with("LICENSE")
                    || n.starts_with("LICENCE")
                    || n.starts_with("COPYING")
                    || n.starts_with("NOTICE"))
        })
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// Rust crates the `start` package is built from (normal and build
/// dependencies, no dev ones) – of the workspace at `manifest`, for
/// `platform` only if given.
fn rust_components(manifest: &Path, start: &str, platform: Option<&str>) -> Result<Vec<Component>> {
    let mut args = vec!["metadata", "--format-version", "1", "--locked"];
    if let Some(p) = platform {
        args.extend(["--filter-platform", p]);
    }
    let out = std::process::Command::new("cargo")
        .args(&args)
        .arg("--manifest-path")
        .arg(manifest)
        .output()
        .context("cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let packages: BTreeMap<&str, &serde_json::Value> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| (p["id"].as_str().unwrap_or_default(), p))
        .collect();
    let nodes: BTreeMap<&str, &serde_json::Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| (n["id"].as_str().unwrap_or_default(), n))
        .collect();
    let start = packages
        .values()
        .find(|p| p["name"] == start)
        .and_then(|p| p["id"].as_str())
        .with_context(|| format!("package {start}"))?;
    let mut seen = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        for d in nodes
            .get(id)
            .and_then(|n| n["deps"].as_array())
            .into_iter()
            .flatten()
        {
            let shipped = d["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null() || k["kind"] == "build");
            if shipped && let Some(dep) = d["pkg"].as_str() {
                todo.push(dep);
            }
        }
    }
    let mut out = Vec::new();
    for id in seen {
        let p = packages[id];
        // The workspace's own crates are covered by Ancilo's license.
        if p["source"].is_null() {
            continue;
        }
        let dir = Path::new(p["manifest_path"].as_str().unwrap_or_default())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        out.push(Component {
            name: p["name"].as_str().unwrap_or_default().to_string(),
            version: p["version"].as_str().unwrap_or_default().to_string(),
            license: p["license"].as_str().unwrap_or_default().to_string(),
            texts: license_texts(&dir),
        });
    }
    Ok(out)
}

/// npm packages in the app's production bundle.
fn npm_components(root: &Path) -> Result<Vec<Component>> {
    let lock: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("app/package-lock.json"),
    )?)?;
    let mut out = Vec::new();
    for (path, p) in lock["packages"].as_object().into_iter().flatten() {
        if path.is_empty() || p["dev"] == true {
            continue;
        }
        let dir = root.join("app").join(path);
        out.push(Component {
            name: path
                .trim_start_matches("node_modules/")
                .replace("/node_modules/", " > "),
            version: p["version"].as_str().unwrap_or_default().to_string(),
            license: p["license"].as_str().unwrap_or_default().to_string(),
            texts: license_texts(&dir),
        });
    }
    Ok(out)
}

/// THIRD_PARTY_NOTICES for the release packages.
///
/// Everything shipped: the CLI and daemon (`ancilo`), the native app
/// (`app/src-tauri`, macOS only – its own workspace: Tauri, WebKit glue,
/// plugins) and the web UI's npm packages. A crate in both is listed once.
/// Code ported from other projects: folder under `third_party/ported`, and
/// what it is.
const PORTED: &[(&str, &str)] = &[
    (
        "atomic-agent",
        "Atomic Agent (AtomicBot-ai/atomic-agent, result compressor, MIT) – crates/agent/src/compress.rs",
    ),
    (
        "langchain-textsplitters",
        "@langchain/textsplitters 0.0.0 (RecursiveCharacterTextSplitter, MIT) – crates/docs/src/split.rs",
    ),
    (
        "anything-llm",
        "AnythingLLM (Mintplex-Labs/anything-llm, TextSplitter, MIT) – crates/docs/src/split.rs",
    ),
    (
        "jan",
        "Jan (janhq/jan, preview states, Apache-2.0; This product includes software developed by Menlo Research (https://menlo.ai)) – app/src/state/preview.ts",
    ),
];

fn notices(root: &Path) -> Result<String> {
    let mut all = rust_components(&root.join("Cargo.toml"), "ancilo", None)?;
    all.extend(rust_components(
        &root.join("app/src-tauri/Cargo.toml"),
        "ancilo-app",
        Some("aarch64-apple-darwin"),
    )?);
    all.extend(npm_components(root)?);
    let mut seen = BTreeSet::new();
    all.retain(|c| seen.insert((c.name.clone(), c.version.clone())));
    let missing: Vec<String> = all
        .iter()
        .filter(|c| c.license.is_empty())
        .map(|c| format!("{} {}", c.name, c.version))
        .collect();
    if !missing.is_empty() {
        bail!(
            "components without license information: {}",
            missing.join(", ")
        );
    }
    let mut s = String::from(
        "Ancilo includes the following third-party components.\n\nllama.cpp (ggml-org/llama.cpp, MIT) – see LICENSE-llama.cpp\n\n",
    );
    for c in &all {
        s.push_str(&format!("{} {} – {}\n", c.name, c.version, c.license));
    }
    // Code translated from other projects (third_party/ported).
    s.push_str("\nPorted source code (translated to Rust)\n");
    let ported = root.join("third_party/ported");
    let mut ported_texts = Vec::new();
    for (dir, what) in PORTED {
        s.push_str(&format!("{what}\n"));
        let text = std::fs::read_to_string(ported.join(dir).join("LICENSE"))
            .with_context(|| format!("third_party/ported/{dir}/LICENSE"))?;
        ported_texts.push((what, text));
    }
    s.push_str("\n\nLicense texts\n=============\n");
    for (what, text) in ported_texts {
        s.push_str(&format!("\n---- {what} ----\n\n{}\n", text.trim_end()));
    }
    let mut printed = BTreeSet::new();
    for c in &all {
        for text in &c.texts {
            // Identical texts (e.g. the Apache license) are printed once per crate family.
            if printed.insert((
                c.name.split('-').next().unwrap_or_default().to_string(),
                text.len(),
            )) {
                s.push_str(&format!(
                    "\n---- {} {} ----\n\n{}\n",
                    c.name,
                    c.version,
                    text.trim_end()
                ));
            }
        }
    }
    Ok(s)
}

// ---- documentation links (M9-AC-05) -------------------------------------------

/// GitHub's anchor for a heading.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

fn anchors(text: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    let mut fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            let h = h.trim_start_matches('#');
            if h.starts_with(' ') {
                let s = slug(h);
                let n = seen.entry(s.clone()).or_insert(0);
                out.insert(if *n == 0 {
                    s.clone()
                } else {
                    format!("{s}-{n}")
                });
                *n += 1;
            }
        }
    }
    out
}

/// Markdown links `[text](target)` outside code.
fn links(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fence = false;
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        // Inline code may contain brackets that are no links.
        let plain: String = line.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        let mut rest = plain.as_str();
        while let Some(at) = rest.find("](") {
            let after = &rest[at + 2..];
            if let Some(end) = after.find(')') {
                out.push((i + 1, after[..end].to_string()));
                rest = &after[end..];
            } else {
                break;
            }
        }
    }
    out
}

/// Relative links and anchors in the public docs and the project notes.
fn check_links(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for dir in ["docs", ".devnotes", ".github"] {
        collect_md(&root.join(dir), &mut files);
    }
    for f in [
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "PRIVACY.md",
        "CHANGELOG.md",
    ] {
        if root.join(f).exists() {
            files.push(root.join(f));
        }
    }
    let mut broken = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();
        for (line, target) in links(&text) {
            let target = target.split_whitespace().next().unwrap_or_default();
            if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let (path, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target, None),
            };
            let dest = if path.is_empty() {
                file.clone()
            } else {
                file.parent().unwrap_or(root).join(path)
            };
            if !dest.exists() {
                broken.push(format!("{rel}:{line}: missing {target}"));
                continue;
            }
            if let Some(a) = anchor
                && dest.extension().is_some_and(|x| x == "md")
                && !a.is_empty()
                && !anchors(&std::fs::read_to_string(&dest)?).contains(a)
            {
                broken.push(format!("{rel}:{line}: no anchor #{a} in {path}"));
            }
        }
    }
    Ok(broken)
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if p.extension().is_some_and(|x| x == "md")
            // Templates (`_vorlage.md`) contain placeholder links.
            && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('_'))
        {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn sample(status: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".devnotes/specs/M7-sample.md",
            &format!(
                "# M7 – Sample\n\n- **Status:** {status}\n\n| ID | Absicht | Prüfung |\n|---|---|---|\n| M7-AC-01 | first | a |\n| M7-AC-02 | second | b |\n| ~~M7-AC-03~~ | dropped | c |\n| M7-AC-04 | needs key | *blockiert* (Voraussetzung) |\n"
            ),
        );
        dir
    }

    // covers: M0-AC-05
    #[test]
    fn trace_fails_for_uncovered_criteria_of_active_milestones() {
        let dir = sample("In Arbeit");
        write(
            dir.path(),
            "crates/x/src/lib.rs",
            "// covers: M7-AC-01\n#[test]\nfn t() {}\n",
        );
        let specs = load_specs(dir.path()).unwrap();
        let cov = scan_coverage(dir.path()).unwrap();
        let report = trace(&specs, &cov);
        assert_eq!(report.missing, vec!["M7-AC-02".to_string()]);
        assert!(report.unknown.is_empty());
    }

    #[test]
    fn a_status_with_a_remark_is_still_active() {
        let dir = sample("In Arbeit (offen: AC-04)");
        let specs = load_specs(dir.path()).unwrap();
        assert!(specs[0].active());
        assert!(
            !trace(&specs, &scan_coverage(dir.path()).unwrap())
                .missing
                .is_empty()
        );
    }

    // covers: M0-AC-05
    #[test]
    fn trace_ignores_draft_milestones_and_flags_unknown_ids() {
        let dir = sample("Entwurf");
        write(
            dir.path(),
            "crates/x/src/lib.rs",
            "// covers: M7-AC-99\nfn t() {}\n",
        );
        let specs = load_specs(dir.path()).unwrap();
        let report = trace(&specs, &scan_coverage(dir.path()).unwrap());
        assert!(report.missing.is_empty());
        assert_eq!(report.unknown[0].criterion, "M7-AC-99");
    }

    fn relative_links(markdown: &str) -> Vec<String> {
        let mut links = Vec::new();
        // Outside code: `[text](…)` in backticks is no link.
        for (_, found) in super::links(markdown) {
            let target = found.split('#').next().unwrap_or_default().trim();
            if !target.is_empty()
                && !target.contains("://")
                && !target.starts_with("mailto:")
                && !target.contains(' ')
            {
                links.push(target.to_string());
            }
        }
        links
    }

    // covers: M0-AC-08
    #[test]
    fn agent_docs_exist_and_all_relative_links_resolve() {
        let root = repo_root();
        // The agent instructions and project notes live on the maintainer's
        // machine (not in the public repository) – checked where they are.
        if root.join(".devnotes").is_dir() {
            assert!(root.join("AGENTS.md").is_file());
            assert!(root.join("CLAUDE.md").is_file());
            assert!(root.join(".devnotes/status.md").is_file());
        }
        let mut files: Vec<PathBuf> = ["AGENTS.md", "CLAUDE.md", "README.md"]
            .iter()
            .map(|f| root.join(f))
            .filter(|p| p.is_file())
            .collect();
        for dir in [".devnotes", "docs"] {
            let mut stack = vec![root.join(dir)];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "md") {
                        files.push(p);
                    }
                }
            }
        }
        let mut broken = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            if f.file_name().is_some_and(|n| n == "_vorlage.md") {
                continue; // template with placeholder links
            }
            for link in relative_links(&text) {
                if !f.parent().unwrap().join(&link).exists() {
                    broken.push(format!(
                        "{} → {link}",
                        f.strip_prefix(&root).unwrap().display()
                    ));
                }
            }
        }
        assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
    }

    // covers: M0-AC-01, M0-AC-02
    #[test]
    fn ci_runs_the_gate_on_linux_macos_and_offline() {
        let ci = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap();
        assert!(ci.contains("verify-linux:") && ci.contains("run: just verify"));
        assert!(ci.contains("verify-macos:") && ci.contains("macos-latest"));
        assert!(ci.contains("verify-offline:") && ci.contains("iptables -A OUTPUT -j REJECT"));
        let justfile = std::fs::read_to_string(repo_root().join("justfile")).unwrap();
        assert!(justfile.contains("verify: fmt-check lint test app-check trace"));
    }

    // covers: M0-AC-09
    #[test]
    fn parses_criteria_states_and_test_functions() {
        let dir = sample("In Arbeit");
        write(
            dir.path(),
            "crates/models/tests/a.rs",
            "// covers: M7-AC-01, M7-AC-02\n#[tokio::test]\nasync fn does_things() {}\n",
        );
        let m = &load_specs(dir.path()).unwrap()[0];
        assert!(m.criteria.iter().any(|c| c.id == "M7-AC-03" && c.dropped));
        assert!(m.criteria.iter().any(|c| c.id == "M7-AC-04" && c.blocked));
        let cov = scan_coverage(dir.path()).unwrap();
        assert_eq!(cov.len(), 2);
        assert_eq!(cov[0].test_fn.as_deref(), Some("does_things"));
        assert_eq!(cov[0].krate, "ancilo-models");
        write(
            dir.path(),
            "target/nextest/ci/junit.xml",
            r#"<testsuites><testsuite><testcase name="tests::does_things" classname="ancilo-models::a"><failure/></testcase><testcase name="x::ok" classname="ancilo-core"/></testsuite></testsuites>"#,
        );
        let r = junit_results(dir.path());
        assert_eq!(
            r.get(&("ancilo-models".into(), "does_things".into())),
            Some(&false)
        );
        assert_eq!(r.get(&("ancilo-core".into(), "ok".into())), Some(&true));
    }

    // covers: M9-AC-05
    #[test]
    fn broken_links_and_anchors_are_found() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "docs/a.md",
            "# Top\n\n## Über uns & mehr\n\n[ok](b.md#setup) [self](#über-uns--mehr) [bad](b.md#nope) [gone](c.md) [web](https://x.y) `[not](a link)`\n",
        );
        write(
            dir.path(),
            "docs/b.md",
            "# B\n\n## Setup\n\n```\n# not a heading\n```\n",
        );
        let broken = check_links(dir.path()).unwrap();
        assert_eq!(
            broken,
            vec![
                "docs/a.md:5: no anchor #nope in b.md".to_string(),
                "docs/a.md:5: missing c.md".to_string()
            ]
        );
    }

    #[test]
    fn notices_cover_every_shipped_component_with_a_license() {
        let root = repo_root();
        if !root.join("app/node_modules").exists() {
            return; // needs `just app-deps`
        }
        let text = notices(&root).unwrap();
        // The native app's own crates too (its own workspace).
        for name in [
            "axum ",
            "rusqlite ",
            "tokio ",
            "react ",
            "@xterm/xterm ",
            "tauri ",
            "wry ",
            "tao ",
            "tauri-plugin-updater ",
            "tauri-plugin-notification ",
        ] {
            assert!(text.contains(&format!("\n{name}")), "{name} missing");
        }
        // Ported code names its origin, with its license text.
        assert!(text.contains("Atomic Agent (AtomicBot-ai/atomic-agent"));
        assert!(text.contains("Copyright (c) 2026 Atomic Bot"));
        assert!(text.contains("Copyright (c) 2023 LangChain"));
        // Dev-only packages are not shipped.
        assert!(!text.contains("\nvitest "));
        assert!(!text.contains("\ninsta "));
        // A crate both workspaces use is listed once.
        let list = text.split("License texts").next().unwrap();
        let lines: Vec<&str> = list.lines().filter(|l| l.contains(" – ")).collect();
        let unique: BTreeSet<&&str> = lines.iter().collect();
        assert_eq!(unique.len(), lines.len(), "a component listed twice");
        assert!(text.contains("Apache License"));
    }

    fn gate_fixture(status: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".devnotes/specs/M1-x.md",
            &format!(
                "# M1 – X\n\n- **Status:** {status}\n\n| ID | Absicht | Prüfung |\n|---|---|---|\n| M1-AC-01 | a | b |\n| M1-AC-02 | c | d |\n"
            ),
        );
        write(
            dir.path(),
            "crates/core/src/lib.rs",
            "// covers: M1-AC-01\n#[test]\nfn works() {}\n",
        );
        write(
            dir.path(),
            "app/e2e/ui.spec.ts",
            "// covers: M1-AC-02\ntest(\"the UI & more\", async () => {});\n",
        );
        dir
    }

    fn results(dir: &Path, name: &str, commit: &str, xml: &str) -> PathBuf {
        let d = dir.join("results").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("COMMIT"), commit).unwrap();
        std::fs::write(d.join("junit.xml"), xml).unwrap();
        d.join("junit.xml")
    }

    const RUST_OK: &str = r#"<testsuites><testsuite><testcase name="tests::works" classname="ancilo-core"/></testsuite></testsuites>"#;
    const UI_OK: &str = r#"<testsuites><testsuite><testcase name="the UI &amp; more" classname="ui.spec.ts"></testcase><testcase name="the UI &amp; more" classname="ui.spec.ts"></testcase></testsuite></testsuites>"#;

    // covers: M9-AC-07
    #[test]
    fn the_release_gate_passes_only_when_everything_passed_on_this_commit() {
        let dir = gate_fixture("In Arbeit");
        let r = results(dir.path(), "rust", "abc", RUST_OK);
        let u = results(dir.path(), "ui", "abc", UI_OK);
        assert_eq!(
            release_gate(dir.path(), "abc", &[r.clone(), u.clone()]).unwrap(),
            Vec::<String>::new()
        );
        // A missing result blocks.
        let p = release_gate(dir.path(), "abc", std::slice::from_ref(&r)).unwrap();
        assert_eq!(
            p,
            ["M1-AC-02: the UI & more (app/e2e/ui.spec.ts) has no result"]
        );
        // A failure in one browser blocks.
        let failed = UI_OK.replacen("></testcase>", "><failure message=\"x\"/></testcase>", 1);
        let u2 = results(dir.path(), "ui2", "abc", &failed);
        assert_eq!(
            release_gate(dir.path(), "abc", &[r.clone(), u2]).unwrap(),
            ["M1-AC-02: the UI & more failed"]
        );
        // Skipped counts as not passed.
        let skipped = RUST_OK.replace("/>", "><skipped/></testcase>");
        let r2 = results(dir.path(), "rust2", "abc", &skipped);
        assert_eq!(
            release_gate(dir.path(), "abc", &[r2, u.clone()]).unwrap(),
            ["M1-AC-01: works was skipped"]
        );
        // Results of another commit block.
        let p = release_gate(dir.path(), "def", &[r, u]).unwrap();
        assert_eq!(p.len(), 2);
        assert!(p[0].contains("not def"));
    }

    // covers: M9-AC-07
    #[test]
    fn blocked_criteria_and_drafts_block_the_release() {
        let dir = gate_fixture("Entwurf");
        let r = results(dir.path(), "rust", "abc", RUST_OK);
        let u = results(dir.path(), "ui", "abc", UI_OK);
        assert_eq!(
            release_gate(dir.path(), "abc", &[r.clone(), u.clone()]).unwrap(),
            ["M1: milestone is still a draft (Entwurf)"]
        );
        let spec = dir.path().join(".devnotes/specs/M1-x.md");
        let text = std::fs::read_to_string(&spec)
            .unwrap()
            .replace("Entwurf", "In Arbeit")
            .replace("| c | d |", "| c | *blockiert* (key) |");
        std::fs::write(&spec, text).unwrap();
        assert_eq!(
            release_gate(dir.path(), "abc", &[r, u]).unwrap(),
            ["M1-AC-02: blocked"]
        );
    }

    // covers: M9-AC-08
    #[test]
    #[ignore = "release check: two full release builds (just repro-check)"]
    fn reproducible_release_builds() {
        let work = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(repo_root().join("packaging/repro-check.sh"))
            .arg(work.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "two builds of the same commit differ – see the output above"
        );
    }

    #[test]
    fn the_eval_report_takes_the_newest_report_per_suite_model_and_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let w = |name: &str, v: serde_json::Value| {
            std::fs::write(dir.path().join(name), v.to_string()).unwrap()
        };
        w(
            "a.json",
            serde_json::json!({"suite": "tool-calling", "target": {"model": "m1", "label": "reliability=off"}, "started_at": "2026-01-01T00:00:00Z", "repeats": 1, "tasks": [1, 2], "success_rate": 0.5, "latency_p50_ms": 1500}),
        );
        w(
            "b.json",
            serde_json::json!({"suite": "tool-calling", "target": {"model": "m1", "label": "reliability=off"}, "started_at": "2026-02-01T00:00:00Z", "repeats": 2, "tasks": [1, 2], "success_rate": 0.75, "latency_p50_ms": 1200}),
        );
        w(
            "c.json",
            serde_json::json!({"suite": "delegation", "model": "m2", "started_at": "2026-02-01T00:00:00Z", "repeats": 1, "tasks": [1, 2, 3], "success_rate": 1.0, "search": true}),
        );
        let md = eval_report(dir.path(), "test machine").unwrap();
        assert!(md.contains("| m1 | off | 75 % | 4 | 1.2 s |"), "{md}");
        assert!(!md.contains("50 %"));
        assert!(md.contains("| m2 | with search | 100 % | 3 | – |"));
        assert!(md.contains("## Method"));
    }

    // covers: M9-AC-07
    #[test]
    fn a_pre_release_names_its_known_gaps_and_nothing_else_passes() {
        let accepted =
            parse_accepted("# v0.1.0\nM9-AC-04  Homebrew follows later\n\nM9-AC-01 no VM yet\n")
                .unwrap();
        assert_eq!(accepted.len(), 2);
        assert!(
            parse_accepted("M2-AC-04").is_err(),
            "a gap needs its reason"
        );
        let (open, known) = split_accepted(
            vec![
                "M9-AC-04: brew_installs failed".into(),
                "M9-AC-01: fresh_mac has no result".into(),
                "M1-AC-03: add_model failed".into(),
            ],
            &accepted,
        );
        assert_eq!(open, ["M1-AC-03: add_model failed"]);
        assert_eq!(
            known,
            [
                "M9-AC-04: brew_installs failed – Homebrew follows later",
                "M9-AC-01: fresh_mac has no result – no VM yet"
            ]
        );
    }

    // covers: M9-AC-07
    #[test]
    fn a_release_is_one_command_from_a_checked_commit_and_only_a_draft() {
        let script = std::fs::read_to_string(repo_root().join("packaging/release.sh")).unwrap();
        let body = script
            .split("\npreflight\n")
            .nth(1)
            .expect("the steps at the end");
        let at = |s: &str| {
            body.find(s)
                .unwrap_or_else(|| panic!("release.sh does not run {s}"))
        };
        // Order: preflight → build (signed, notarized) → green CI → draft.
        assert!(
            script.contains("\npreflight\nbuild\nci_green\ndraft"),
            "the steps, in order"
        );
        assert!(at("build") < at("ci_green") && at("ci_green") < at("draft"));
        for must in [
            "git status --porcelain",          // only from a clean checkout
            "merge-base --is-ancestor",        // only a pushed commit
            "CHANGELOG.md has no section",     // notes for every release
            "packaging/sign.sh \"$dist\" cli", // the CLI signed and notarized
            "packaging/sign.sh \"$dist\" app", // the app signed, notarized, checked like a download
            "latest.json",                     // the updater finds it
            "conclusion\" = \"success\"",      // never from a red CI
            "--draft",                         // the maintainer publishes
        ] {
            assert!(script.contains(must), "release.sh lacks {must}");
        }
        let sign = std::fs::read_to_string(repo_root().join("packaging/sign.sh")).unwrap();
        assert!(sign.contains("not releasing unsigned") && sign.contains("exit 1"));
    }

    // covers: M11-AC-01
    /// Nothing sends past the one door (`ancilo-net`, which logs what leaves
    /// this computer): a request elsewhere in the product's code is a
    /// failure – except the clients that only ever talk to this computer.
    #[test]
    fn nothing_sends_past_the_log_of_what_left() {
        // Only this computer: the daemon (CLI, evals) and the local model's health.
        const LOOPBACK: &[&str] = &[
            "crates/cli/src/client.rs",
            "crates/eval/src/coding.rs",
            "crates/eval/src/delegation.rs",
            "crates/eval/src/lib.rs",
            "crates/models/src/llama.rs",
        ];
        // Build a client and hand it to `ancilo_net::Net` at once.
        const WRAPPED: &[&str] = &[
            "crates/gateway/src/lib.rs",
            "crates/models/src/hf.rs",
            "crates/models/src/manager.rs",
            "crates/web/src/lib.rs",
            "crates/web/src/net.rs",
        ];
        let root = repo_root();
        let mut found = Vec::new();
        let mut stack = vec![root.join("crates")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                let rel = p
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if p.is_dir() {
                    let skip = ["crates/net", "crates/testkit", "crates/e2e", "crates/xtask"]
                        .contains(&rel.as_str())
                        || rel.ends_with("/tests")
                        || rel.ends_with("/target");
                    if !skip {
                        stack.push(p);
                    }
                    continue;
                }
                if !rel.ends_with(".rs") || !rel.contains("/src/") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                // The product's code – not its tests.
                let code = text.split("#[cfg(test)]").next().unwrap_or_default();
                for (n, line) in code.lines().enumerate() {
                    let sends = line.contains(".send()") || line.contains(".execute(req");
                    let builds =
                        line.contains("Client::new()") || line.contains("Client::builder()");
                    let ok = LOOPBACK.contains(&rel.as_str())
                        || (builds && !sends && WRAPPED.contains(&rel.as_str()));
                    if (sends || builds) && !ok {
                        found.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "requests past ancilo-net (the user's log of what left this computer):\n{}",
            found.join("\n")
        );
    }
}
