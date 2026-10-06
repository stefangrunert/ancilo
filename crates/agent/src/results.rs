//! Tool results in a long task: shortened by what kind of output they are,
//! and kept whole so the agent can read them again.
//!
//! A conversation outgrows the model's context; older tool output then has
//! to get shorter. What survives a cut decides whether the agent still
//! knows what failed. Before, every cut kept the start – the end of a test
//! run (its verdict, the failing test) was the first thing to go, and what
//! was cut was gone for good.
//!
//! Now:
//! - **The cut fits the output** ([`Shape`]): a log keeps its error lines
//!   and its end, an ordered listing its header and first rows, a text its
//!   start. Built on the port of Atomic Agent's compressor ([`crate::compress`]).
//! - **Status and origin stay**: every tool message carries, besides its
//!   text, which tool it was and whether it failed (`"ancilo"` – never sent
//!   to the model). A shortened result says so in its first line, taken
//!   from that record, not guessed from the words.
//! - **Nothing is lost**: a long result is kept whole in the conversation's
//!   [`ResultStore`] under a short id; the shortened text names it, and
//!   `read_result` shows any part of it again – only in this conversation,
//!   never another's, and gone with the conversation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::compress::{self, Options, Overflow, chars, head, last};

/// The key of a tool message's record (`{"ancilo": {"tool", "error", "id"}}`).
/// It stays in the conversation and is removed from what the model gets.
pub const META: &str = "ancilo";

/// What a result says about itself (from the tool's structured result).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub tool: String,
    #[serde(default)]
    pub error: bool,
    /// The whole result is kept under this id (long results only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Meta {
    pub fn of(message: &Value) -> Option<Self> {
        serde_json::from_value(message.get(META)?.clone()).ok()
    }
}

/// Which part of an output matters once it has to get shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A command's output, a test run: the error lines and the end.
    Log,
    /// An ordered listing (matches, files, search results): the header and
    /// the first rows.
    Listing,
    /// A file, a document, anything else: the start.
    Text,
}

impl Shape {
    pub fn of(tool: &str, error: bool) -> Self {
        // A failure is about its error lines and its end, whatever the tool.
        if error {
            return Shape::Log;
        }
        match tool {
            "bash" => Shape::Log,
            "grep" | "glob" | "search" | "list_files" | "search_documents" | "web_search" => {
                Shape::Listing
            }
            _ => Shape::Text,
        }
    }
}

/// Results shorter than this are never cut (the smallest cut), so they are
/// not kept apart either.
pub const KEEP_FROM_CHARS: usize = 150;

/// `text` in at most `budget` characters, the part that matters for its
/// kind first, with a first line that says what it was, whether it failed
/// and where the whole of it is.
pub fn shorten(meta: &Meta, text: &str, budget: usize) -> String {
    if chars(text) <= budget {
        return text.to_string();
    }
    let status = if meta.error { " · FAILED" } else { "" };
    let whole = match &meta.id {
        Some(id) => format!(" · read_result {id}"),
        None => String::new(),
    };
    // Shortened before (at the tool, or by an earlier cut): its first line
    // is the label made then – replaced, and its count of the whole kept.
    let (text, lines, signals) = match earlier_label(meta, text, status) {
        Some(e) => (e.rest, e.lines, e.signals),
        None => (text, text.lines().count(), None),
    };
    let label = format!(
        "[{}{status} · shortened from {lines} lines{whole}]",
        meta.tool
    );
    if chars(&label) + 20 > budget {
        return head(&label, budget).to_string();
    }
    let room = budget - chars(&label) - 1;
    let body = match Shape::of(&meta.tool, meta.error) {
        Shape::Log => log_body(text, room, signals),
        Shape::Listing => {
            let end = closing_line(text, room);
            let start = compress::compress(
                text,
                false,
                Options {
                    max_summary_len: room - chars(&end),
                    max_tail_lines: usize::MAX,
                    overflow: Overflow::Head,
                },
            )
            .summary;
            format!("{start}{end}")
        }
        Shape::Text => text_body(text, room),
    };
    format!("{label}\n{body}")
}

/// What an earlier cut of this result put on top: its label (`[bash ·
/// FAILED · shortened from 3002 lines…]`) with the count of the whole, and the
/// signals it counted on the whole. Both are kept, not counted again on
/// what is left.
struct Earlier<'a> {
    rest: &'a str,
    lines: usize,
    signals: Option<&'a str>,
}

fn earlier_label<'a>(meta: &Meta, text: &'a str, status: &str) -> Option<Earlier<'a>> {
    let (first, rest) = text.split_once('\n')?;
    let count = first
        .strip_prefix(&format!("[{}{status} · shortened from ", meta.tool))?
        .strip_suffix(']')?;
    let (count, after) = count.split_once(" lines")?;
    // Only a label this result could have had: no id, or its own.
    if let Some(id) = &meta.id
        && !after.is_empty()
        && after != format!(" · read_result {id}")
    {
        return None;
    }
    let lines = count.parse().ok()?;
    let (signals, rest) = match rest.split_once('\n') {
        Some((s, r)) if s.starts_with("(signals: ") => (Some(s), r),
        _ => (None, rest),
    };
    Some(Earlier {
        rest,
        lines,
        signals,
    })
}

/// The last line of a listing or a text when it is a note, not one more
/// row – where tools say what they left out ("… 428 more", "read with
/// offset=281") – and fits a fifth of the room: `"\n<line>"`, or nothing.
fn closing_line(text: &str, room: usize) -> String {
    let text = text.trim_end();
    let Some((before, l)) = text.rsplit_once('\n') else {
        return String::new();
    };
    let row = before
        .lines()
        .find(|r| !r.trim().is_empty())
        .unwrap_or_default();
    let shape = |s: &str| -> String {
        plain(s)
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .take(12)
            .collect()
    };
    if l.trim().is_empty() || chars(l) + 1 > room / 5 || shape(l) == shape(row) {
        return String::new();
    }
    format!("\n{l}")
}

/// A text: its start – and, in a text of repeated lines, first the lines
/// that do not fit its pattern (what is said once among the same said a
/// hundred times), then the start with the room left; its closing note.
fn text_body(text: &str, room: usize) -> String {
    let end = closing_line(text, room);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let body = if end.is_empty() {
        &lines[..]
    } else {
        &lines[..lines.len() - 1]
    };
    let odd = off_pattern(body, &[]);
    if odd.is_empty() {
        let mut h = head(text, (room - chars(&end)).saturating_sub(15)).to_string();
        // End on a whole line where one ends near the cut.
        if let Some(i) = h.rfind('\n')
            && chars(&h[i..]) < 80
        {
            h.truncate(i);
        }
        return format!("{h}\n… [truncated]{end}");
    }
    // Each gap costs a "…" line; the end a marker.
    let mut avail = room.saturating_sub(chars(&end) + 15);
    let mut picked = std::collections::BTreeSet::new();
    for i in odd.into_iter().chain(0..body.len()) {
        let cost = chars(body[i]) + 3;
        if picked.contains(&i) || cost > avail {
            continue;
        }
        avail -= cost;
        picked.insert(i);
    }
    let mut out = String::new();
    let mut next = 0;
    for i in picked {
        if i > next {
            out.push_str("…\n");
        }
        out.push_str(body[i]);
        out.push('\n');
        next = i + 1;
    }
    format!("{out}… [truncated]{end}")
}

/// A log: its signals counted (the port's log summary), the lines that
/// name an error or a failure – each with the line after it, where the
/// details usually are (expected/got) – and, in a log of repeated lines,
/// those that do not fit its pattern; then its end (the port's tail cut).
/// Picked from both ends first; with room to spare, its start too.
fn log_body(text: &str, room: usize, signals: Option<&str>) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if all.is_empty() {
        return String::new();
    }
    // A small room: the findings before the counts and the long end.
    let small = room < 300;
    let mut out = String::new();
    let s = compress::summarise_log(text);
    if let Some(signals) = signals.filter(|_| !small) {
        out.push_str(signals);
        out.push('\n');
    } else if !small && s.fail_count + s.error_lines > 0 {
        out.push_str(&format!(
            "(signals: {} fail, {} error, {} pass)\n",
            s.fail_count, s.error_lines, s.pass_count
        ));
    }
    // The end: half the room – in a small one only the last line (the
    // exit code, the verdict).
    let last_line = all[all.len() - 1];
    let end_room = if small {
        chars(last_line) + 1
    } else {
        room / 2
    };
    let mut tail_from = all.len();
    let mut size = 0;
    for (i, l) in all.iter().enumerate().rev() {
        size += chars(l) + 1;
        if size > end_room {
            break;
        }
        tail_from = i;
    }
    // The findings before the end.
    let mut distinct = std::collections::HashSet::new();
    let keys: Vec<usize> = (0..tail_from)
        .filter(|&i| is_key_line(all[i]))
        .filter(|&i| distinct.insert(plain(all[i])))
        .collect();
    let rare = off_pattern(&all[..tail_from], &keys);
    let keys_set: std::collections::HashSet<usize> = keys.iter().copied().collect();
    // From both ends of the log, then those in between; then the odd ones.
    let mut order = Vec::new();
    let (mut a, mut b) = (0usize, keys.len());
    while a < b {
        order.push(keys[a]);
        a += 1;
        if a < b {
            b -= 1;
            order.push(keys[b]);
        }
    }
    order.extend(rare.iter().copied());
    let room_for_findings = room.saturating_sub(chars(&out) + end_room.min(room / 2) + 1);
    let mut picked: std::collections::BTreeMap<usize, String> = Default::default();
    let mut used = 0;
    let mut left_out = 0;
    for i in order {
        let mut entry = vec![(i, format!("! {}", head(plain(all[i]), 200)))];
        // The detail after a finding (not one of the findings itself).
        if i + 1 < tail_from && !keys_set.contains(&(i + 1)) && !picked.contains_key(&(i + 1)) {
            entry.push((i + 1, format!("  {}", head(plain(all[i + 1]), 200))));
        }
        let cost: usize = entry
            .iter()
            .filter(|(j, _)| !picked.contains_key(j))
            .map(|(_, l)| chars(l) + 1)
            .sum();
        if picked.contains_key(&i) {
            continue;
        }
        if used + cost > room_for_findings {
            left_out += usize::from(keys_set.contains(&i));
            continue;
        }
        used += cost;
        for (j, l) in entry {
            picked.entry(j).or_insert(l);
        }
    }
    for l in picked.values() {
        out.push_str(l);
        out.push('\n');
    }
    if left_out > 0 && !small {
        out.push_str(&format!("! … {left_out} more such lines\n"));
    }
    let rest = room.saturating_sub(chars(&out));
    // A large room: the start of the log as well (the command, the setup).
    let start_room = if rest >= 4000 { rest / 5 } else { 0 };
    if start_room > 0 {
        let mut h = head(text, start_room).to_string();
        if let Some(i) = h.rfind('\n') {
            h.truncate(i);
        }
        out.push_str(&h);
        out.push('\n');
    }
    let tail_room = room.saturating_sub(chars(&out));
    let tail = if small {
        // The last line whole, or as much of its end as fits.
        last(last_line, tail_room).to_string()
    } else {
        compress::compress(
            text,
            false,
            Options {
                max_summary_len: tail_room,
                max_tail_lines: usize::MAX,
                overflow: Overflow::Tail,
            },
        )
        .summary
    };
    out.push_str(&tail);
    // Never over the room, whatever the parts added up to.
    if chars(&out) > room {
        return last(&out, room).to_string();
    }
    out
}

/// A line as found, without what an earlier cut put before it.
fn plain(line: &str) -> &str {
    line.trim().trim_start_matches("! ")
}

/// In a log of repeated lines (`check_0001 ... ok` a thousand times), the
/// lines that fit none of its patterns – a finding without an error word
/// (`required=17 observed=3 decision=hold`). Patterns: the line with its
/// digits left out. Nothing in a log without repetition.
fn off_pattern(lines: &[&str], skip: &[usize]) -> Vec<usize> {
    let skip: std::collections::HashSet<&usize> = skip.iter().collect();
    let pattern = |l: &str| -> String {
        plain(l)
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .collect::<String>()
    };
    let mut count: HashMap<String, usize> = HashMap::new();
    for l in lines {
        *count.entry(pattern(l)).or_default() += 1;
    }
    let most = count.values().copied().max().unwrap_or(0);
    // Repetitive enough to tell the odd ones out.
    if lines.len() < 20 || most * 3 < lines.len() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !skip.contains(i) && count[&pattern(l)] == 1)
        .map(|(i, _)| i)
        .collect()
}

/// A line that names an error or a failure (the port's markers, and the
/// failure words of the log summary).
fn is_key_line(line: &str) -> bool {
    static KEY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            // ASCII word boundaries: the words are ASCII, and Unicode ones
            // would make the scan of a large log ten times slower.
            r"(?i)error:|failed:|traceback \(most recent call last\)|assertionerror|exception:|(?-u:\b)(panicked|error|exception|fail|failed|fehler|fehlgeschlagen)(?-u:\b)",
        )
        .expect("valid")
    });
    KEY.is_match(line)
}

// ---- keeping results whole ---------------------------------------------------

/// One result kept whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub id: String,
    pub tool: String,
    pub error: bool,
    /// The call's arguments (shortened), to tell results apart.
    pub arguments: String,
    pub text: String,
}

/// At most this much is kept per conversation; the oldest results go first.
pub const MAX_KEPT_BYTES: u64 = 32 * 1024 * 1024;
/// One result is kept up to this size.
pub const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024;

/// Where a conversation's results are kept whole: in memory (a delegated
/// task) or in the conversation's folder (a chat session, which outlives
/// the daemon – and goes with the conversation).
#[derive(Clone, Default)]
pub struct ResultStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    next: u32,
    memory: HashMap<String, Kept>,
}

impl std::fmt::Debug for ResultStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResultStore")
    }
}

impl ResultStore {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Kept in `dir` (created when the first result comes).
    pub fn at(dir: &Path) -> Self {
        let next = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_prefix('r')?
                    .strip_suffix(".json")?
                    .parse::<u32>()
                    .ok()
            })
            .max()
            .unwrap_or(0);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: Some(dir.to_path_buf()),
                next,
                memory: HashMap::new(),
            })),
        }
    }

    /// Keeps a result whole; its id. Short results are not kept (`None`).
    pub fn keep(&self, tool: &str, arguments: &Value, error: bool, text: &str) -> Option<String> {
        if chars(text) <= KEEP_FROM_CHARS || tool == "read_result" {
            return None;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.next += 1;
        let id = format!("r{}", inner.next);
        let mut text = text.to_string();
        if text.len() > MAX_RESULT_BYTES {
            let mut end = MAX_RESULT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n… (only the first 2 MB were kept)");
        }
        let kept = Kept {
            id: id.clone(),
            tool: tool.to_string(),
            error,
            arguments: head(&arguments.to_string(), 300).to_string(),
            text,
        };
        match inner.dir.clone() {
            Some(dir) => {
                let written = std::fs::create_dir_all(&dir).and_then(|()| {
                    std::fs::write(
                        dir.join(format!("{id}.json")),
                        serde_json::to_vec(&kept).unwrap_or_default(),
                    )
                });
                if let Err(e) = written {
                    tracing::warn!(error = %e, "a tool result could not be kept");
                    return None;
                }
                prune(&dir);
            }
            None => {
                inner.memory.insert(id.clone(), kept);
                // The same bound in memory: the oldest go.
                let mut total: u64 = inner.memory.values().map(|k| k.text.len() as u64).sum();
                while total > MAX_KEPT_BYTES {
                    let Some(oldest) = inner
                        .memory
                        .keys()
                        .min_by_key(|k| k[1..].parse::<u32>().unwrap_or(0))
                        .cloned()
                    else {
                        break;
                    };
                    if let Some(k) = inner.memory.remove(&oldest) {
                        total -= k.text.len() as u64;
                    }
                }
            }
        }
        Some(id)
    }

    /// A result of this conversation, if it is (still) kept.
    pub fn get(&self, id: &str) -> Option<Kept> {
        // Only ids this store hands out: `r` and a number – never a path.
        let n: u32 = id.strip_prefix('r')?.parse().ok()?;
        let inner = self.inner.lock().unwrap();
        match &inner.dir {
            Some(dir) => {
                let bytes = std::fs::read(dir.join(format!("r{n}.json"))).ok()?;
                serde_json::from_slice(&bytes).ok()
            }
            None => inner.memory.get(&format!("r{n}")).cloned(),
        }
    }
}

/// The oldest results go while the folder holds more than [`MAX_KEPT_BYTES`].
fn prune(dir: &Path) {
    let mut files: Vec<(u32, PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e
                .file_name()
                .to_str()?
                .strip_prefix('r')?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some((n, e.path(), e.metadata().ok()?.len()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    files.sort_by_key(|f| f.0);
    for (_, path, size) in files {
        if total <= MAX_KEPT_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

// ---- reading a result again --------------------------------------------------

/// Lines shown by one `read_result` at most, and characters.
const READ_LINES: usize = 150;
const READ_MAX_LINES: usize = 400;
const READ_MAX_CHARS: usize = 12_000;

/// The `read_result` tool (offered when results are kept).
pub fn definition() -> Value {
    json!({"type": "function", "function": {
        "name": "read_result",
        "description": "Read an earlier tool result again that was shortened (its first line says `read_result r…`). Give `query` to see only the lines that contain it, or `from_line` to see a part.",
        "parameters": {"type": "object", "properties": {
            "id": {"type": "string", "description": "The result's id, e.g. \"r3\""},
            "query": {"type": "string", "description": "Only lines containing this text (case-insensitive)"},
            "from_line": {"type": "integer", "description": "First line to show (1 = the start; negative: from the end)"},
            "lines": {"type": "integer", "description": "How many lines (default 150, at most 400)"}
        }, "required": ["id"]}
    }})
}

#[derive(Deserialize)]
struct ReadArgs {
    id: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    from_line: Option<i64>,
    #[serde(default)]
    lines: Option<usize>,
}

/// Runs `read_result`: (text, is_error).
pub fn read(store: &ResultStore, args: &Value) -> (String, bool) {
    let a: ReadArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return (format!("invalid arguments: {e}"), true),
    };
    let id = a.id.trim().trim_start_matches('"').trim_end_matches('"');
    let Some(kept) = store.get(id) else {
        return (
            format!("there is no result {id} in this conversation (or it is no longer kept)"),
            true,
        );
    };
    let lines: Vec<&str> = kept.text.lines().collect();
    let total = lines.len();
    let status = if kept.error { " · FAILED" } else { "" };
    let label = format!("[{} · {}{status} · {total} lines]", kept.id, kept.tool);
    let mut out = String::new();
    let mut shown = 0usize;
    if let Some(q) = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let q = q.to_lowercase();
        let hits: Vec<(usize, &str)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&q))
            .map(|(i, l)| (i, *l))
            .collect();
        out.push_str(&format!("{label} {} lines contain {q:?}\n", hits.len()));
        for (i, l) in hits.iter().take(READ_MAX_LINES) {
            let row = format!("{:>6}\t{}\n", i + 1, head(l, 500));
            if chars(&out) + chars(&row) > READ_MAX_CHARS {
                out.push_str("… (more – narrow the query or use from_line)\n");
                break;
            }
            out.push_str(&row);
            shown += 1;
        }
        let _ = shown;
        return (out, false);
    }
    let count = a.lines.unwrap_or(READ_LINES).clamp(1, READ_MAX_LINES);
    let start = match a.from_line {
        Some(n) if n < 0 => total.saturating_sub(n.unsigned_abs() as usize),
        Some(n) => (n.max(1) as usize - 1).min(total),
        None => 0,
    };
    let end = (start + count).min(total);
    out.push_str(&format!("{label} lines {}–{end}\n", start + 1));
    for (i, l) in lines[start..end].iter().enumerate() {
        let row = format!("{:>6}\t{}\n", start + i + 1, head(l, 500));
        if chars(&out) + chars(&row) > READ_MAX_CHARS {
            out.push_str(&format!(
                "… (more from line {} – use from_line)\n",
                start + i + 1
            ));
            break;
        }
        out.push_str(&row);
    }
    if end < total && !out.ends_with("from_line)\n") {
        out.push_str(&format!(
            "… {} more lines (from_line {})\n",
            total - end,
            end + 1
        ));
    }
    (out, false)
}

/// The messages as the model gets them: without Ancilo's records.
pub fn for_model(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m.get(META) {
            Some(_) => {
                let mut m = m.clone();
                if let Some(o) = m.as_object_mut() {
                    o.remove(META);
                }
                m
            }
            None => m.clone(),
        })
        .collect()
}

/// The record of a tool message – or, for one from before records existed,
/// what can be told: the tool by its call's id, without a status.
pub fn meta_in(messages: &[Value], index: usize) -> Meta {
    if let Some(m) = Meta::of(&messages[index]) {
        return m;
    }
    let call = &messages[index]["tool_call_id"];
    let tool = messages[..index]
        .iter()
        .rev()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| &c["id"] == call && !call.is_null())
        .and_then(|c| c["function"]["name"].as_str())
        .unwrap_or("tool")
        .to_string();
    Meta {
        tool,
        error: false,
        id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(tool: &str, error: bool, id: Option<&str>) -> Meta {
        Meta {
            tool: tool.into(),
            error,
            id: id.map(String::from),
        }
    }

    fn test_log(fail_at: usize, n: usize) -> String {
        let mut l: Vec<String> = (0..n).map(|i| format!("test_case_{i:04} ... ok")).collect();
        l[fail_at] = format!("test_case_{fail_at:04} ... FAILED: expected 12.50, got 12.49");
        l.push(format!("== {} passed, 1 failed ==", n - 1));
        l.push("[exit code 1]".into());
        l.join("\n")
    }

    #[test]
    fn a_log_keeps_its_failure_from_anywhere_and_its_verdict() {
        for fail_at in [0, 150, 299] {
            let log = test_log(fail_at, 300);
            for budget in [600, 800, 2000] {
                let s = shorten(&meta("bash", true, Some("r7")), &log, budget);
                assert!(chars(&s) <= budget, "{budget}: {}", chars(&s));
                assert!(
                    s.starts_with("[bash · FAILED · shortened from 302 lines · read_result r7]")
                );
                assert!(
                    s.contains(&format!("test_case_{fail_at:04} ... FAILED")),
                    "{fail_at}/{budget}:\n{s}"
                );
                assert!(s.contains("[exit code 1]"), "{budget}");
            }
        }
    }

    #[test]
    fn the_old_cut_lost_what_the_new_one_keeps() {
        // What Ancilo did before: the start, nothing else.
        let log = test_log(250, 300);
        let old: String = log.chars().take(800).collect();
        assert!(!old.contains("FAILED"));
        let new = shorten(&meta("bash", true, None), &log, 800);
        assert!(new.contains("FAILED") && new.contains("1 failed"));
    }

    #[test]
    fn a_listing_keeps_its_first_rows() {
        let hits: String = (0..500)
            .map(|i| format!("src/file_{i:03}.rs:{i}: fn handler_{i}()\n"))
            .collect();
        let s = shorten(&meta("grep", false, Some("r2")), &hits, 800);
        assert!(chars(&s) <= 800);
        assert!(s.contains("src/file_000.rs") && s.contains("src/file_005.rs"));
        assert!(!s.contains("src/file_499.rs"));
    }

    #[test]
    fn short_results_stay_as_they_are_and_tiny_budgets_hold() {
        assert_eq!(shorten(&meta("bash", true, None), "boom", 10), "boom");
        let long = "x".repeat(1000);
        for b in [0, 5, 60, 150] {
            assert!(chars(&shorten(&meta("read_file", false, Some("r1")), &long, b)) <= b);
        }
    }

    #[test]
    fn a_large_output_is_shortened_quickly() {
        // 2 MB, the most a result is kept: well under the budget of 50 ms
        // (debug build: a generous bound).
        let log: String = (0..60_000)
            .map(|i| {
                if i % 997 == 0 {
                    format!("case {i} FAILED: x\n")
                } else {
                    format!("case {i} ok – fine\n")
                }
            })
            .collect();
        let started = std::time::Instant::now();
        for budget in [20_000, 2000, 800, 150] {
            let s = shorten(&meta("bash", true, Some("r1")), &log, budget);
            assert!(chars(&s) <= budget);
        }
        let ms = started.elapsed().as_millis();
        assert!(ms < 2000, "{ms} ms");
    }

    #[test]
    fn unicode_is_never_split() {
        let text = "Prüfung ✓ 测试 🚀 Fehler: Größe falsch\n".repeat(300);
        for b in [150, 151, 152, 600, 2000] {
            let s = shorten(&meta("bash", true, Some("r1")), &text, b);
            assert!(chars(&s) <= b);
            assert!(s.contains("Fehler: Größe falsch"), "{b}");
        }
    }

    #[test]
    fn results_are_kept_by_conversation_and_read_in_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResultStore::at(&dir.path().join("results"));
        let log = test_log(120, 300);
        let id = store
            .keep("bash", &json!({"command": "./check.sh"}), true, &log)
            .unwrap();
        assert_eq!(id, "r1");
        assert_eq!(store.keep("bash", &json!({}), false, "short"), None);
        let (found, err) = read(&store, &json!({"id": "r1", "query": "failed"}));
        assert!(!err);
        assert!(found.contains("test_case_0120 ... FAILED"), "{found}");
        assert!(found.contains("   121\t"), "line numbers: {found}");
        let (tail, _) = read(&store, &json!({"id": "r1", "from_line": -2}));
        assert!(tail.contains("[exit code 1]") && tail.contains("lines 301–302"));
        // After a restart: the same results, new ids after the old ones.
        let again = ResultStore::at(&dir.path().join("results"));
        assert!(
            read(&again, &json!({"id": "r1"}))
                .0
                .contains("test_case_0000")
        );
        assert_eq!(
            again
                .keep("grep", &json!({}), false, &"y".repeat(400))
                .as_deref(),
            Some("r2")
        );
        // Another conversation's store does not know them; ids are never paths.
        let other = ResultStore::at(&dir.path().join("other"));
        for id in ["r1", "../results/r1", "r1.json", "/etc/passwd", "r-1"] {
            let (text, err) = read(&other, &json!({"id": id}));
            assert!(err, "{id}: {text}");
        }
        assert!(read(&again, &json!({"id": "../results/r1"})).1);
    }

    #[test]
    fn the_oldest_results_go_beyond_the_bound() {
        let store = ResultStore::in_memory();
        let big = "z".repeat(MAX_RESULT_BYTES);
        for _ in 0..(MAX_KEPT_BYTES as usize / MAX_RESULT_BYTES + 2) {
            store.keep("bash", &json!({}), false, &big);
        }
        assert!(store.get("r1").is_none() && store.get("r2").is_none());
        assert!(store.get("r18").is_some());
    }

    #[test]
    fn records_are_not_sent_to_the_model() {
        let m = vec![
            json!({"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "grep"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "x", META: {"tool": "grep", "error": true}}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "y"}),
        ];
        assert!(for_model(&m).iter().all(|m| m.get(META).is_none()));
        assert_eq!(meta_in(&m, 1), meta("grep", true, None));
        // From before records existed: the tool by its call.
        assert_eq!(meta_in(&m, 2), meta("grep", false, None));
    }
}
