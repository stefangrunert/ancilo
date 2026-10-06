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
    let picked = &joined(c, picked);
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

/// Passages next to each other in the same part, as one: a requirement
/// across their border is there when both are (review 1, finding 18). Made
/// from the part's own text, so the overlap is not doubled – and only over
/// whitespace between them (review 2, finding 27).
fn joined(c: &Case, picked: &[Passage]) -> Vec<Passage> {
    let mut sorted: Vec<&Passage> = picked.iter().collect();
    sorted.sort_by_key(|p| (p.document.clone(), p.part, p.start.unwrap_or(0)));
    let text_of = |p: &Passage| -> Option<Vec<char>> {
        let d = c.documents.iter().find(|d| d.name == p.document)?;
        Some(d.parts.get(p.part)?.text.chars().collect())
    };
    // Where a passage stands in the part: from its start, line by line in
    // order (a paragraph passage has its lines trimmed: only whitespace may
    // lie between them). Not found so: it stays on its own.
    let span = |p: &Passage, chars: &[char]| -> Option<(usize, usize)> {
        let start = p.start?;
        let mut at = start;
        for line in p.text.lines() {
            let line: Vec<char> = line.trim().chars().collect();
            if line.is_empty() {
                continue;
            }
            while at < chars.len() && chars[at].is_whitespace() {
                at += 1;
            }
            if chars.get(at..at + line.len())? != line.as_slice() {
                return None;
            }
            at += line.len();
        }
        Some((start, at))
    };
    let mut out: Vec<Passage> = Vec::new();
    let mut end = 0usize;
    for p in sorted {
        let Some(chars) = text_of(p) else {
            out.push(p.clone());
            continue;
        };
        let Some((start, stop)) = span(p, &chars) else {
            out.push(p.clone());
            continue;
        };
        if let Some(last) = out.last_mut()
            && last.document == p.document
            && last.part == p.part
            && let Some(from) = last.start
            // Overlapping, or apart by whitespace only: nothing between them
            // is taken from the document that no passage holds.
            && (start <= end || chars[end..start].iter().all(|c| c.is_whitespace()))
        {
            end = end.max(stop);
            last.text = chars[from..end].iter().collect();
            continue;
        }
        end = stop;
        out.push(Passage {
            text: chars[start..stop].iter().collect(),
            ..p.clone()
        });
    }
    // Each passage on its own, too.
    out.extend(picked.iter().cloned());
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
        let mut taken: Vec<Passage> = Vec::new();
        let mut hit = None;
        for (_, p) in ranked {
            used += p.text.chars().count();
            taken.push(p.clone());
            if holds(c, req, p) || joined(c, &taken).iter().any(|j| holds(c, req, j)) {
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
    // Ancilo's own way passes every development case – a regression fails.
    if std::env::var("FPL01_CASES").is_err() {
        assert_eq!(passed[0], cases.len(), "bestand lost a development case");
    }
    if let Ok(out) = std::env::var("FPL01_REPORT") {
        let names: Vec<&str> = vs.iter().map(|v| v.0).collect();
        std::fs::write(out, serde_json::to_string_pretty(&json!({"cases": path.display().to_string(), "variants": names, "passed": passed, "within_2000": to_hit.iter().map(|v| within(v, 2000)).collect::<Vec<_>>(), "within_4000": to_hit.iter().map(|v| within(v, 4000)).collect::<Vec<_>>(), "median_chars_to_hit": medians, "rows": rows})).unwrap()).unwrap();
    }
}

// covers: FPL-01 (the measure itself, review 2 finding 27)
#[test]
fn joining_passages_neither_makes_up_nor_loses_text() {
    let case = |text: &str| Case {
        id: "m".into(),
        group: String::new(),
        documents: vec![Doc {
            name: "d.txt".into(),
            parts: vec![RawPart {
                page: None,
                sheet: None,
                text: text.into(),
            }],
        }],
        question: String::new(),
        required: Vec::new(),
        required_locator: Vec::new(),
    };
    let at = |start: usize, text: &str| Passage {
        document: "d.txt".into(),
        at: None,
        part: 0,
        start: Some(start),
        text: text.into(),
    };
    // What lies between two passages is in neither.
    let c = case("ABnoCD");
    let j = joined(&c, &[at(0, "AB"), at(4, "CD")]);
    assert!(
        !j.iter().any(|p| holds(&c, "no", p)),
        "{:?}",
        j.iter().map(|p| &p.text).collect::<Vec<_>>()
    );
    // Overlapping passages of repeated lines cover all they hold.
    let text = "repeated text line\n".repeat(80) + "needle";
    let c = case(&text);
    let chars: Vec<char> = text.chars().collect();
    let cut = |a: usize, b: usize| at(a, &chars[a..b].iter().collect::<String>());
    let j = joined(&c, &[cut(0, 683), cut(589, 1272), cut(1178, chars.len())]);
    assert!(j.iter().any(|p| p.text == text), "the whole text is held");
    // Apart by whitespace only: one.
    let c = case("Frist von drei\n\nMonaten");
    let j = joined(&c, &[at(0, "Frist von drei"), at(16, "Monaten")]);
    assert!(j.iter().any(|p| holds(&c, "drei Monaten", p)));
}
