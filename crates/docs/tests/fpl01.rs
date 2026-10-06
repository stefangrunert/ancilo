//! FPL-01: does a passage selection keep what a question needs? The cases
//! of `evals/fpl01/` (long documents, a question, the text the answer
//! needs) through Ancilo's selection with different segmentations, at the
//! same budget (`PASSAGE_BUDGET`) and the same scoring (BM25):
//!
//! - `bestand`: paragraphs joined to about 700 characters (before),
//! - `port`: LangChain's recursive splitter at AnythingLLM's sizes,
//! - `port+kopf`: the same, each passage also found by its document's name
//!   and page or sheet (AnythingLLM's chunk header),
//! - further sizes of the port, to choose from on the development set.
//!
//! `FPL01_CASES=<file>` runs another set, `FPL01_REPORT=<file>` writes JSON.

use std::path::PathBuf;

use ancilo_docs::{Cut, Header, Locator, PASSAGE_BUDGET, Part, Passage, Segmenter, ranked, select};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Doc {
    name: String,
    parts: Vec<RawPart>,
}

#[derive(Deserialize)]
struct RawPart {
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    sheet: Option<String>,
    text: String,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    #[serde(default)]
    group: String,
    documents: Vec<Doc>,
    question: String,
    required: Vec<String>,
    #[serde(default)]
    required_locator: Vec<Value>,
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn variants() -> Vec<(&'static str, Segmenter)> {
    let r = |size, overlap, header| Segmenter {
        cut: Cut::Recursive { size, overlap },
        header,
    };
    let p = |header| Segmenter {
        cut: Cut::Paragraphs,
        header,
    };
    vec![
        ("bestand", p(Header::None)),
        ("port", r(1000, 20, Header::None)),
        ("port+kopf", r(1000, 20, Header::Prepend)),
        ("700/100", r(700, 100, Header::None)),
        ("700+boost", r(700, 100, Header::Boost)),
        ("absatz+boost", p(Header::Boost)),
    ]
}

/// What a selection misses of a case (empty: everything needed is there).
fn misses(c: &Case, picked: &[Passage]) -> Vec<String> {
    let mut out = Vec::new();
    for req in &c.required {
        let n = norm(req);
        let holding: Vec<&Passage> = picked
            .iter()
            .filter(|p| norm(&p.text).contains(&n))
            .collect();
        if holding.is_empty() {
            out.push(format!("lost {req:?}"));
            continue;
        }
        if !holding.iter().any(|p| holds(c, req, p)) {
            out.push(format!("{req:?} not from where the case says it is"));
        }
    }
    out
}

/// Whether a passage holds a requirement where the case says it is.
fn holds(c: &Case, req: &str, p: &Passage) -> bool {
    if !norm(&p.text).contains(&norm(req)) {
        return false;
    }
    // Where the answer stands: one of the places the case names.
    c.required_locator.is_empty()
        || c.required_locator.iter().any(|loc| {
            let at = match (loc["page"].as_u64(), loc["sheet"].as_str()) {
                (Some(n), _) => Some(Locator::Page(n as u32)),
                (_, Some(s)) => Some(Locator::Sheet(s.into())),
                _ => None,
            };
            p.document == loc["document"].as_str().unwrap_or_default()
                && (at.is_none() || p.at == at)
        })
}

/// How many characters of the best-ranked passages it takes until every
/// requirement is in (lower: found with less room – a folder of many
/// documents, a small model); `None`: never.
fn chars_to_hit(c: &Case, ranked: &[(usize, Passage)]) -> Option<usize> {
    let mut worst = 0;
    for req in &c.required {
        let mut used = 0;
        let mut hit = None;
        for (_, p) in ranked {
            used += p.text.chars().count();
            if holds(c, req, p) {
                hit = Some(used);
                break;
            }
        }
        worst = worst.max(hit?);
    }
    Some(worst)
}

fn cases() -> (PathBuf, Vec<Case>) {
    let path = std::env::var("FPL01_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/fpl01/dev-cases.json")
        });
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (path, serde_json::from_str(&text).expect("cases"))
}

// covers: FPL-01 (passage selection)
#[test]
fn which_segmentation_keeps_what_the_question_needs() {
    let (path, cases) = cases();
    let vs = variants();
    let mut passed = vec![0usize; vs.len()];
    let mut to_hit: Vec<Vec<Option<usize>>> = vec![Vec::new(); vs.len()];
    let mut rows = Vec::new();
    println!(
        "FPL-01 · {} · {} cases · budget {PASSAGE_BUDGET}",
        path.display(),
        cases.len()
    );
    print!("{:<12} {:<18}", "case", "group");
    for (n, _) in &vs {
        print!(" {n:>13}");
    }
    println!();
    for c in &cases {
        let docs: Vec<(String, Vec<Part>)> = c
            .documents
            .iter()
            .map(|d| {
                let parts = d
                    .parts
                    .iter()
                    .map(|p| Part {
                        at: match (&p.page, &p.sheet) {
                            (Some(n), _) => Some(Locator::Page(*n)),
                            (_, Some(s)) => Some(Locator::Sheet(s.clone())),
                            _ => None,
                        },
                        text: p.text.clone(),
                    })
                    .collect();
                (d.name.clone(), parts)
            })
            .collect();
        let mut row = json!({"id": c.id, "group": c.group});
        print!("{:<12} {:<18}", c.id, &c.group[..c.group.len().min(18)]);
        for (i, (n, seg)) in vs.iter().enumerate() {
            let picked = select(&docs, &c.question, PASSAGE_BUDGET, *seg);
            let m = misses(c, &picked);
            if m.is_empty() {
                passed[i] += 1;
            }
            let hit = chars_to_hit(c, &ranked(&docs, &c.question, *seg));
            if !c.required.is_empty() {
                to_hit[i].push(hit);
            }
            let mark = match (m.is_empty(), hit) {
                (true, Some(h)) if !c.required.is_empty() => format!("ok {h}"),
                (true, _) => "ok".to_string(),
                (false, Some(h)) => format!("✗{} {h}", m.len()),
                (false, None) => format!("✗{} –", m.len()),
            };
            print!(" {mark:>13}");
            row[n] = json!({"passed": m.is_empty(), "misses": m, "passages": picked.len(), "chars_to_hit": hit,
                            "chars": picked.iter().map(|p| p.text.chars().count()).sum::<usize>()});
        }
        println!();
        rows.push(row);
    }
    print!("{:<31}", "passed (12 000):");
    for p in &passed {
        print!(" {p:>13}");
    }
    println!();
    // Found within a tighter room, and the median room needed.
    let within =
        |v: &Vec<Option<usize>>, b: usize| v.iter().filter(|h| h.is_some_and(|h| h <= b)).count();
    for b in [2000, 4000] {
        print!("{:<31}", format!("found within {b}:"));
        for v in &to_hit {
            print!(" {:>13}", within(v, b));
        }
        println!();
    }
    print!("{:<31}", "median chars to hit:");
    let medians: Vec<Option<usize>> = to_hit
        .iter()
        .map(|v| {
            let mut x: Vec<usize> = v.iter().map(|h| h.unwrap_or(usize::MAX)).collect();
            x.sort();
            x.get(x.len() / 2).copied().filter(|m| *m != usize::MAX)
        })
        .collect();
    for m in &medians {
        print!(" {:>13}", m.map_or("–".to_string(), |m| m.to_string()));
    }
    println!();
    if let Ok(out) = std::env::var("FPL01_REPORT") {
        let names: Vec<&str> = vs.iter().map(|v| v.0).collect();
        std::fs::write(out, serde_json::to_string_pretty(&json!({"cases": path.display().to_string(), "variants": names, "passed": passed, "within_2000": to_hit.iter().map(|v| within(v, 2000)).collect::<Vec<_>>(), "within_4000": to_hit.iter().map(|v| within(v, 4000)).collect::<Vec<_>>(), "median_chars_to_hit": medians, "rows": rows})).unwrap()).unwrap();
    }
}
