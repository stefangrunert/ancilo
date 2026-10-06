//! Choosing the passages that answer the question – locally, with a small
//! keyword ranking (BM25 over word stems). Measured with a prototype before it
//! was built: the article's introduction always goes along (it holds the core
//! facts), then the best-matching passages.

use std::collections::HashSet;

/// Passages are cut at paragraph ends to about this many characters …
pub const PASSAGE_CHARS: usize = 700;
/// … the first page's introduction may be longer.
pub const LEAD_CHARS: usize = 1600;

/// Lower-case word stems (the first six letters: "Einwohnerzahl" meets "Einwohner").
pub fn stems(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase().chars().take(6).collect())
        .collect()
}

/// Paragraphs joined into passages of about [`PASSAGE_CHARS`].
pub fn passages(text: &str) -> Vec<String> {
    cut(text, ' ')
}

/// The same passages, their lines kept apart (`\n` where [`passages`] has
/// a space – the same cuts, the same lengths).
pub fn passages_by_line(text: &str) -> Vec<String> {
    cut(text, '\n')
}

fn cut(text: &str, join: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for para in text
        .split('\n')
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.starts_with("=="))
    {
        if !buf.is_empty() && buf.chars().count() + para.chars().count() > PASSAGE_CHARS {
            out.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push(join);
        }
        buf.push_str(para);
        // A single overlong paragraph is cut.
        while buf.chars().count() > PASSAGE_CHARS * 2 {
            let cut: String = buf.chars().take(PASSAGE_CHARS).collect();
            buf = buf.chars().skip(PASSAGE_CHARS).collect();
            out.push(cut);
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// The introduction of a text: up to its first section heading.
pub fn lead(text: &str) -> String {
    let head = text.split("\n==").next().unwrap_or(text).trim();
    head.chars().take(LEAD_CHARS).collect()
}

/// BM25 scores of `docs` for `query`.
pub fn scores(docs: &[String], query: &str) -> Vec<f64> {
    let q: HashSet<String> = stems(query).into_iter().collect();
    let docs: Vec<Vec<String>> = docs.iter().map(|d| stems(d)).collect();
    let n = docs.len().max(1) as f64;
    let avg = (docs.iter().map(Vec::len).sum::<usize>() as f64 / n).max(1.0);
    let df = |w: &String| docs.iter().filter(|d| d.contains(w)).count() as f64;
    docs.iter()
        .map(|d| {
            let len = d.len() as f64;
            q.iter()
                .map(|w| {
                    let tf = d.iter().filter(|x| *x == w).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let idf = (1.0 + (n - df(w) + 0.5) / (df(w) + 0.5)).ln();
                    idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * len / avg))
                })
                .sum()
        })
        .collect()
}

/// One page's text for the model: the introduction (first page only) and its best passages.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    /// Index of the page.
    pub page: usize,
    pub text: String,
}

/// Picks at most `n` passages over all pages: the first page's introduction,
/// then the best-ranked ones, in page order for reading.
pub fn choose(pages: &[String], query: &str, n: usize) -> Vec<Chosen> {
    let mut all: Vec<(usize, String)> = Vec::new();
    for (i, text) in pages.iter().enumerate() {
        all.extend(passages(text).into_iter().map(|p| (i, p)));
    }
    let mut chosen: Vec<(usize, usize, String)> = Vec::new();
    let lead_text = pages.first().map(|t| lead(t)).filter(|l| !l.is_empty());
    if let Some(l) = &lead_text {
        chosen.push((0, 0, l.clone()));
    }
    let texts: Vec<String> = all.iter().map(|(_, t)| t.clone()).collect();
    let s = scores(&texts, query);
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by(|a, b| s[*b].total_cmp(&s[*a]).then(a.cmp(b)));
    for i in order {
        if chosen.len() >= n {
            break;
        }
        let (page, text) = &all[i];
        // Skip what the introduction already holds.
        if lead_text
            .as_ref()
            .is_some_and(|l| *page == 0 && l.contains(text.as_str()))
        {
            continue;
        }
        chosen.push((*page, i + 1, text.clone()));
    }
    chosen.sort_by_key(|(page, pos, _)| (*page, *pos));
    chosen
        .into_iter()
        .map(|(page, _, text)| Chosen { page, text })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_forms_meet_at_their_stem() {
        assert_eq!(stems("Einwohnerzahl Einwohner"), ["einwoh", "einwoh"]);
        assert_eq!(stems("Höhe der Zugspitze"), ["höhe", "der", "zugspi"]);
    }

    #[test]
    fn the_introduction_and_the_matching_passage_are_chosen() {
        let article = format!(
            "Bergen ist eine Stadt in Norwegen.\n\n== Geschichte ==\n{}\n\n== Bevölkerung ==\nDie Stadt Bergen hat 274.589 Einwohner (Stand 2026).\n\n== Kultur ==\n{}",
            "Hanse und Handel. ".repeat(60),
            "Musik und Theater. ".repeat(60)
        );
        let other = "Bergen ist auch ein Ort in Niedersachsen.".to_string();
        let got = choose(&[article, other], "Einwohnerzahl Bergen", 3);
        assert_eq!(
            got[0],
            Chosen {
                page: 0,
                text: "Bergen ist eine Stadt in Norwegen.".into()
            }
        );
        assert!(got.iter().any(|c| c.text.contains("274.589")), "{got:?}");
        assert!(got.len() <= 3);
    }

    #[test]
    fn long_texts_become_passages_of_bounded_size() {
        let text = "Satz. ".repeat(1000);
        let p = passages(&text);
        assert!(p.len() > 3);
        assert!(p.iter().all(|x| x.chars().count() <= PASSAGE_CHARS * 2));
        assert!(lead(&text).chars().count() <= LEAD_CHARS);
    }
}
