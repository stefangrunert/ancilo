//! Splitting a document's text into passages – a port of LangChain's
//! `RecursiveCharacterTextSplitter`, as AnythingLLM uses it.
//!
//! Sources: AnythingLLM `server/utils/TextSplitter/index.js` (Mintplex-Labs/
//! anything-llm `ead123f07befc7167d05f160dade56aed5fa7c10`, MIT, © Mintplex
//! Labs Inc.) wraps `@langchain/textsplitters` 0.0.0 (`dist/text_splitter.js`,
//! npm integrity
//! `sha512-3hPesWomnmVeYMppEGYbyv0v/sRUugUdlFBNn9m1ueJYHAIKbvCErkWxNUH3guyKKYgJVrkvZoQxcd9faucSaw==`,
//! MIT, © 2023 LangChain). Ported:
//! `splitOnSeparator`, `mergeSplits`, `joinDocs`, `_splitText` and the line
//! counting of `createDocuments`; AnythingLLM's defaults (1000 characters,
//! 20 overlap) and its idea of a metadata header on every chunk (here: the
//! document's name, used to find the passage, not shown in it). License
//! texts: `third_party/ported/`.
//!
//! Deliberate differences: lengths count characters (JavaScript: UTF-16
//! units); a chunk carries where it starts in the text (characters) and its
//! lines, found the same way `createDocuments` finds them (`indexOf` after
//! the previous chunk) – Ancilo's sources point at that place.

/// AnythingLLM's defaults.
pub const CHUNK_SIZE: usize = 1000;
pub const CHUNK_OVERLAP: usize = 20;

/// `RecursiveCharacterTextSplitter` with `keepSeparator` (its default).
#[derive(Debug, Clone)]
pub struct Splitter {
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub separators: Vec<String>,
}

impl Default for Splitter {
    fn default() -> Self {
        Self::new(CHUNK_SIZE, CHUNK_OVERLAP)
    }
}

/// A passage and where it is in the text it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Where it starts (characters into the text).
    pub start: usize,
    /// Its first and last line (1-based), as `createDocuments` counts them.
    pub lines: (usize, usize),
}

fn len(s: &str) -> usize {
    s.chars().count()
}

impl Splitter {
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        assert!(
            chunk_overlap < chunk_size,
            "Cannot have chunkOverlap >= chunkSize"
        );
        Self {
            chunk_size,
            chunk_overlap,
            separators: ["\n\n", "\n", " ", ""].map(String::from).to_vec(),
        }
    }

    /// `splitText`.
    pub fn split_text(&self, text: &str) -> Vec<String> {
        self.split_with(text, &self.separators)
    }

    /// `_splitText`: the first separator the text contains; pieces still
    /// too long are split again with the next ones.
    fn split_with(&self, text: &str, separators: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut separator = separators.last().cloned().unwrap_or_default();
        let mut next: Option<&[String]> = None;
        for (i, s) in separators.iter().enumerate() {
            if s.is_empty() {
                separator = s.clone();
                break;
            }
            if text.contains(s.as_str()) {
                separator = s.clone();
                next = Some(&separators[i + 1..]);
                break;
            }
        }
        let splits = split_on_separator(text, &separator);
        let mut good: Vec<String> = Vec::new();
        for s in splits {
            if len(&s) < self.chunk_size {
                good.push(s);
            } else {
                if !good.is_empty() {
                    out.extend(self.merge_splits(&good, ""));
                    good.clear();
                }
                match next {
                    None => out.push(s),
                    Some(rest) => out.extend(self.split_with(&s, rest)),
                }
            }
        }
        if !good.is_empty() {
            out.extend(self.merge_splits(&good, ""));
        }
        out
    }

    /// `mergeSplits`: pieces joined up to the chunk size; the next chunk
    /// starts with the last pieces of this one (the overlap).
    fn merge_splits(&self, splits: &[String], separator: &str) -> Vec<String> {
        let mut docs = Vec::new();
        let mut current: std::collections::VecDeque<&str> = Default::default();
        let mut total = 0usize;
        let sep = len(separator);
        for d in splits {
            let l = len(d);
            if total + l + current.len() * sep > self.chunk_size && !current.is_empty() {
                if let Some(doc) = join_docs(current.iter().copied(), separator) {
                    docs.push(doc);
                }
                while total > self.chunk_overlap
                    || (total + l + current.len() * sep > self.chunk_size && total > 0)
                {
                    let Some(first) = current.pop_front() else {
                        break;
                    };
                    total -= len(first);
                }
            }
            current.push_back(d);
            total += l;
        }
        if let Some(doc) = join_docs(current.iter().copied(), separator) {
            docs.push(doc);
        }
        docs
    }

    /// `createDocuments` for one text: the chunks with where they start and
    /// their lines.
    pub fn chunks(&self, text: &str) -> Vec<Chunk> {
        let chars: Vec<char> = text.chars().collect();
        let newlines = |from: usize, to: usize| -> usize {
            chars[from.min(chars.len())..to.min(chars.len())]
                .iter()
                .filter(|c| **c == '\n')
                .count()
        };
        let mut out = Vec::new();
        let mut line = 1usize;
        let mut prev: Option<(usize, usize)> = None; // (start, length)
        for chunk in self.split_text(text) {
            let from = prev.map_or(0, |(s, _)| s + 1);
            let start = index_of(&chars, &chunk, from).unwrap_or(from);
            match prev {
                None => line += newlines(0, start),
                Some((s, l)) => {
                    let end_prev = s + l;
                    if end_prev < start {
                        line += newlines(end_prev, start);
                    } else if end_prev > start {
                        line -= newlines(start, end_prev);
                    }
                }
            }
            let count = chunk.chars().filter(|c| *c == '\n').count();
            out.push(Chunk {
                lines: (line, line + count),
                start,
                text: chunk.clone(),
            });
            line += count;
            prev = Some((start, len(&chunk)));
        }
        out
    }
}

/// `splitOnSeparator` with `keepSeparator`: the separator stays at the
/// start of the piece after it; empty pieces go.
fn split_on_separator(text: &str, separator: &str) -> Vec<String> {
    if separator.is_empty() {
        return text.chars().map(String::from).collect();
    }
    // Like JavaScript's `split(/(?=sep)/)`: before every place the
    // separator starts, overlapping ones too ("\n\n\n" has two).
    let mut out = Vec::new();
    let mut last = 0;
    for (i, _) in text.char_indices() {
        if text[i..].starts_with(separator) {
            if i > last {
                out.push(text[last..i].to_string());
            }
            last = i;
        }
    }
    out.push(text[last..].to_string());
    out.retain(|s| !s.is_empty());
    out
}

/// `joinDocs`: joined and trimmed; nothing when empty.
fn join_docs<'a>(docs: impl Iterator<Item = &'a str>, separator: &str) -> Option<String> {
    let text = docs.collect::<Vec<_>>().join(separator).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `text.indexOf(chunk, from)` in characters.
fn index_of(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() {
        return Some(from.min(chars.len()));
    }
    (from..=chars.len().saturating_sub(n.len())).find(|&i| chars[i..i + n.len()] == n[..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        let s = Splitter::default();
        assert_eq!(s.split_text("Hallo Welt"), vec!["Hallo Welt"]);
    }

    #[test]
    fn paragraphs_then_lines_then_words() {
        let s = Splitter::new(40, 5);
        let text = "Erster Absatz mit einigen Wörtern.\n\nZweiter Absatz, ebenfalls kurz.\n\nDritter Absatz ist etwas länger als der Rest hier.";
        let c = s.split_text(text);
        assert!(c.iter().all(|x| len(x) <= 40), "{c:?}");
        assert_eq!(c[0], "Erster Absatz mit einigen Wörtern.");
        // Nothing lost: every word is in some chunk.
        for w in text.split_whitespace() {
            assert!(c.iter().any(|x| x.contains(w)), "{w}");
        }
    }

    #[test]
    fn chunks_know_where_they_start_and_their_lines() {
        let s = Splitter::new(30, 0);
        let text = "Zeile eins hier\nZeile zwei hier\n\nZeile vier hier ist\nZeile fünf";
        let c = s.chunks(text);
        let chars: Vec<char> = text.chars().collect();
        for k in &c {
            let at: String = chars[k.start..k.start + len(&k.text)].iter().collect();
            assert_eq!(at, k.text, "{k:?}");
        }
        assert_eq!(c[0].lines.0, 1);
        assert!(c.last().unwrap().lines.1 >= 5, "{c:?}");
    }

    #[test]
    fn a_word_longer_than_a_chunk_is_cut_by_characters() {
        let s = Splitter::new(10, 2);
        let c = s.split_text(&"x".repeat(25));
        assert!(c.iter().all(|x| len(x) <= 10), "{c:?}");
        assert!(c.concat().len() >= 25);
    }

    /// Compared with LangChain's own output for the same inputs
    /// (`evals/fpl01/langchain.json`, made by `run-langchain.mjs`).
    #[test]
    fn the_port_matches_langchain() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../evals/fpl01/langchain.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let runs: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        let mut differ = Vec::new();
        for r in &runs {
            let s = Splitter::new(
                r["chunkSize"].as_u64().unwrap() as usize,
                r["chunkOverlap"].as_u64().unwrap() as usize,
            );
            let ours = s.chunks(r["text"].as_str().unwrap());
            let theirs = r["chunks"].as_array().unwrap();
            let same = ours.len() == theirs.len()
                && ours.iter().zip(theirs).all(|(a, b)| {
                    a.text == b["text"].as_str().unwrap()
                        && a.lines.0 as u64 == b["from"].as_u64().unwrap()
                        && a.lines.1 as u64 == b["to"].as_u64().unwrap()
                });
            if !same && r["astral"] != true {
                differ.push(r["id"].as_str().unwrap().to_string());
            }
        }
        assert!(differ.is_empty(), "differs from LangChain: {differ:?}");
    }
}
