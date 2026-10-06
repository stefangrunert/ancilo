//! Evidence for answers from documents (FPL-01): every passage the model
//! gets carries a mark (`[D3]`) that Ancilo – not the model – gave it, and
//! the answer keeps only marks of passages it was given. Each mark stays
//! with the answer: which document (and which folder or attachment), its
//! revision (a hash of its text as read), page or sheet, the passage as it
//! stood, and what to know about the reading (recognized text, cut off).
//!
//! Opened later, a mark shows that passage – and says when the document has
//! changed or is gone since; a new version never stands in for the old one.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extract::{Locator, Part, Warning};
use crate::{Passage, source};

/// Where a document is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// Attached to the conversation.
    Attachment { id: String },
    /// In the chat project's folder (`path` relative to it).
    Folder { folder: PathBuf, path: String },
}

/// One passage an answer could draw on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// The mark (`D3`) – unique in its conversation.
    pub id: String,
    /// The document's name (in a folder: its path there).
    pub document: String,
    pub origin: Origin,
    /// The document's text as it was read (hash) – to tell a later version.
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Locator>,
    /// Which part (page, sheet) by position, and where in it.
    pub part: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    /// The passage as the model got it.
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
    /// The answer named it.
    #[serde(default)]
    pub cited: bool,
}

/// A document's revision: a hash of its text as read (parts and places).
pub fn revision(parts: &[Part]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(serde_json::to_vec(&p.at).unwrap_or_default());
        h.update([0]);
        h.update(p.text.as_bytes());
        h.update([0]);
    }
    hex::encode(&h.finalize()[..8])
}

/// Passages of one source made evidence (marks are given by [`number`]).
pub fn of(
    passages: Vec<Passage>,
    origin: impl Fn(&str) -> (Origin, String, Vec<Warning>),
) -> Vec<Evidence> {
    passages
        .into_iter()
        .map(|p| {
            let (origin, revision, warnings) = origin(&p.document);
            Evidence {
                id: String::new(),
                document: p.document,
                origin,
                revision,
                at: p.at,
                part: p.part,
                start: p.start,
                text: p.text,
                warnings,
                cited: false,
            }
        })
        .collect()
}

/// Gives marks `D<after+1>`, `D<after+2>` … (after the conversation's last).
pub fn number(evidence: &mut [Evidence], after: usize) {
    for (i, e) in evidence.iter_mut().enumerate() {
        e.id = format!("D{}", after + i + 1);
    }
}

/// The highest mark number among `evidence` (0: none).
pub fn last_number<'a>(evidence: impl IntoIterator<Item = &'a Evidence>) -> usize {
    evidence
        .into_iter()
        .filter_map(|e| e.id.strip_prefix('D')?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// The passages as the model gets them: each headed by its mark and source.
pub fn for_model(evidence: &[Evidence]) -> String {
    evidence
        .iter()
        .map(|e| {
            let src = source(&e.document, e.at.as_ref());
            let src = src.trim_start_matches('[').trim_end_matches(']');
            let read = if e.warnings.contains(&Warning::Recognized) {
                " (text recognized from a scan – may have mistakes)"
            } else {
                ""
            };
            format!("[{}] {src}{read}\n{}", e.id, e.text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What the check of an answer's marks found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Marks {
    /// Marks of given passages, in the order they first appear.
    pub cited: Vec<String>,
    /// Marks the model made up (no such passage was given): taken out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

/// The answer with only marks of passages it was given: `[D3]`, `[D3, D5]`
/// and `[D3][D5]` stay (as `[D3]`…); a mark of none of them is taken out.
/// A source named the old way – `[Contract.pdf, page 3]` – becomes the mark
/// of the passage from there, when exactly one was given; else it stays
/// plain text (never a link).
pub fn check(answer: &str, evidence: &mut [Evidence]) -> (String, Marks) {
    let mut marks = Marks::default();
    let mut out = String::with_capacity(answer.len());
    let mut rest = answer;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after
            .find(']')
            .filter(|e| *e <= 200 && !after[..*e].contains('\n'))
        else {
            out.push('[');
            rest = after;
            continue;
        };
        let inner = &after[..end];
        // A link's text (`[D3](…)`) is left as it is.
        let is_link = after[end + 1..].starts_with('(');
        let ids: Vec<&str> = inner.split([',', ';']).map(str::trim).collect();
        if !is_link && ids.iter().all(|s| is_mark(s)) {
            for id in ids {
                let id = id.to_ascii_uppercase();
                if let Some(e) = evidence.iter_mut().find(|e| e.id == id) {
                    e.cited = true;
                    if !marks.cited.contains(&id) {
                        marks.cited.push(id.clone());
                    }
                    out.push_str(&format!("[{id}]"));
                } else if !marks.unknown.contains(&id) {
                    marks.unknown.push(id);
                }
            }
        } else if !is_link && let Some(id) = by_source(inner, evidence) {
            if let Some(e) = evidence.iter_mut().find(|e| e.id == id) {
                e.cited = true;
            }
            if !marks.cited.contains(&id) {
                marks.cited.push(id.clone());
            }
            out.push_str(&format!("[{id}]"));
        } else {
            out.push('[');
            out.push_str(inner);
            out.push(']');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    // A mark taken out leaves no double space before punctuation.
    let tidy = out.replace(" .", ".").replace(" ,", ",").replace("  ", " ");
    (tidy, marks)
}

fn is_mark(s: &str) -> bool {
    let s = s.as_bytes();
    s.len() >= 2 && (s[0] == b'D' || s[0] == b'd') && s[1..].iter().all(u8::is_ascii_digit)
}

/// `Contract.pdf, page 3` → the mark of the one passage given from there.
fn by_source(inner: &str, evidence: &[Evidence]) -> Option<String> {
    let want = format!("[{}]", inner.trim());
    let mut found = evidence
        .iter()
        .filter(|e| source(&e.document, e.at.as_ref()).eq_ignore_ascii_case(&want));
    let first = found.next()?;
    // Several passages from that page: which one is not known.
    found.next().is_none().then(|| first.id.clone())
}

/// How a document stands now, against the evidence taken from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Now {
    /// The same text as when the answer was given.
    Same,
    /// Changed since – the passage shown is the one the answer had.
    Changed,
    /// No longer there (removed, or the conversation's attachment deleted).
    Gone,
}

/// A mark opened: the passage as the answer had it, how its document stands
/// now, and – when unchanged – the text around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Opened {
    pub evidence: Evidence,
    pub now: Now,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Around a passage, this much is shown on either side.
const CONTEXT_CHARS: usize = 300;

/// Opens `e` against its document's text now (`None`: gone).
pub fn open(e: &Evidence, now: Option<&[Part]>) -> Opened {
    let Some(parts) = now else {
        return Opened {
            evidence: e.clone(),
            now: Now::Gone,
            before: None,
            after: None,
        };
    };
    if revision(parts) != e.revision {
        return Opened {
            evidence: e.clone(),
            now: Now::Changed,
            before: None,
            after: None,
        };
    }
    let (mut before, mut after) = (None, None);
    if let (Some(start), Some(p)) = (e.start, parts.get(e.part)) {
        let chars: Vec<char> = p.text.chars().collect();
        let end = (start + e.text.chars().count()).min(chars.len());
        let from = start.saturating_sub(CONTEXT_CHARS);
        let to = (end + CONTEXT_CHARS).min(chars.len());
        let b: String = chars[from..start.min(chars.len())].iter().collect();
        let a: String = chars[end..to].iter().collect();
        before = (!b.trim().is_empty()).then(|| format!("{}{b}", if from > 0 { "…" } else { "" }));
        after = (!a.trim().is_empty())
            .then(|| format!("{a}{}", if to < chars.len() { "…" } else { "" }));
    }
    Opened {
        evidence: e.clone(),
        now: Now::Same,
        before,
        after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, doc: &str, page: u32) -> Evidence {
        Evidence {
            id: id.into(),
            document: doc.into(),
            origin: Origin::Attachment { id: "a-1".into() },
            revision: "r".into(),
            at: Some(Locator::Page(page)),
            part: 0,
            start: Some(0),
            text: "Kündigung: drei Monate.".into(),
            warnings: vec![],
            cited: false,
        }
    }

    #[test]
    fn only_marks_of_given_passages_stay() {
        let mut e = vec![ev("D1", "Vertrag.pdf", 3), ev("D2", "Vertrag.pdf", 4)];
        let (text, m) = check(
            "Drei Monate [D1]. Zum Monatsende [D2, d7]. Laut Anhang [D9].",
            &mut e,
        );
        assert_eq!(text, "Drei Monate [D1]. Zum Monatsende [D2]. Laut Anhang.");
        assert_eq!(m.cited, ["D1", "D2"]);
        assert_eq!(m.unknown, ["D7", "D9"]);
        assert!(e.iter().all(|e| e.cited));
    }

    #[test]
    fn a_source_named_the_old_way_becomes_its_mark_only_when_it_is_one() {
        let mut e = vec![
            ev("D1", "Vertrag.pdf", 3),
            ev("D2", "Vertrag.pdf", 3),
            ev("D3", "Brief.pdf", 1),
        ];
        let (text, m) = check(
            "A [Brief.pdf, page 1]. B [Vertrag.pdf, page 3]. C [Andere.pdf, page 2]. Link [D1](x).",
            &mut e,
        );
        assert_eq!(
            text,
            "A [D3]. B [Vertrag.pdf, page 3]. C [Andere.pdf, page 2]. Link [D1](x)."
        );
        assert_eq!(m.cited, ["D3"]);
        assert!(m.unknown.is_empty());
    }

    #[test]
    fn opened_later_a_changed_or_gone_document_is_said() {
        let parts = vec![Part {
            at: Some(Locator::Page(1)),
            text: "Vorher. Kündigung: drei Monate. Nachher.".into(),
        }];
        let mut e = ev("D1", "Vertrag.pdf", 1);
        e.revision = revision(&parts);
        e.start = Some(8);
        let o = open(&e, Some(&parts));
        assert_eq!(o.now, Now::Same);
        assert_eq!(o.before.as_deref(), Some("Vorher. "));
        assert_eq!(o.after.as_deref(), Some(" Nachher."));
        let changed = vec![Part {
            at: Some(Locator::Page(1)),
            text: "Kündigung: sechs Monate.".into(),
        }];
        let o = open(&e, Some(&changed));
        assert_eq!(o.now, Now::Changed);
        assert_eq!(
            o.evidence.text, "Kündigung: drei Monate.",
            "the passage the answer had"
        );
        assert_eq!(open(&e, None).now, Now::Gone);
    }

    #[test]
    fn marks_continue_through_a_conversation() {
        let mut e = vec![ev("", "a", 1), ev("", "b", 2)];
        number(&mut e, 4);
        assert_eq!((e[0].id.as_str(), e[1].id.as_str()), ("D5", "D6"));
        assert_eq!(last_number(&e), 6);
        assert!(for_model(&e).starts_with("[D5] a, page 1\n"));
    }
}
