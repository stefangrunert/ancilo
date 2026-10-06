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
    /// The text was shortened before (by Ancilo): what the whole was. Only
    /// this record says so – never the text, which a tool's output could
    /// imitate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<Cut>,
}

/// What a shortened result keeps of its whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cut {
    /// Lines of the whole.
    pub lines: usize,
    /// The signals counted on the whole (a log with failures or errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<String>,
}

/// The record of a first cut of `whole`.
pub fn cut_of(whole: &str) -> Cut {
    let s = compress::summarise_log(whole);
    Cut {
        lines: whole.lines().count(),
        signals: (s.fail_count + s.error_lines > 0).then(|| {
            format!(
                "(signals: {} fail, {} error, {} pass)",
                s.fail_count, s.error_lines, s.pass_count
            )
        }),
    }
}

/// Shortens tool message `i` in place – its text and its record (so a
/// later cut knows what the whole was). Whether it got shorter.
pub fn shorten_in(messages: &mut [Value], i: usize, budget: usize) -> bool {
    let Some(text) = messages[i]["content"].as_str().map(String::from) else {
        return false;
    };
    if chars(&text) <= budget {
        return false;
    }
    let mut meta = meta_in(messages, i);
    let short = shorten(&meta, &text, budget);
    if meta.cut.is_none() {
        meta.cut = Some(cut_of(&text));
    }
    messages[i]["content"] = json!(short);
    messages[i][META] = json!(meta);
    true
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
    // Shortened before (the record says so, never the text): its first line
    // and its signals are Ancilo's own – made anew from the record.
    let (text, cut) = match &meta.cut {
        Some(c) => (own_lines_off(meta, c, text), c.clone()),
        None => (text, cut_of(text)),
    };
    let lines = cut.lines;
    let signals = cut.signals.as_deref();
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
            // In a list of look-alike lines, the ones that look otherwise
            // first (`timeout_ms = 0` among five hundred `= 30000`).
            let all: Vec<&str> = text.lines().collect();
            let odd: Vec<usize> = off_pattern(&all, &[]);
            let mut first = String::new();
            for &i in odd.iter().take(20) {
                let l = format!("! {}\n", squeeze(plain(all[i]), 200));
                if chars(&first) + chars(&l) + 1 > room {
                    break;
                }
                first.push_str(&l);
            }
            let room = room - chars(&first);
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
            format!("{first}{start}{end}")
        }
        Shape::Text => text_body(text, room),
    };
    // Never over the budget, whatever the parts added up to (what comes
    // first is what matters most).
    let out = format!("{label}\n{body}");
    if chars(&out) > budget {
        return head(&out, budget).to_string();
    }
    out
}

/// The text of a result shortened before, without what that cut put on
/// top (its label, its signals) – known from the record `cut`.
fn own_lines_off<'a>(meta: &Meta, cut: &Cut, text: &'a str) -> &'a str {
    let mut rest = text;
    if let Some((first, after)) = rest.split_once('\n')
        && first.starts_with(&format!("[{}", meta.tool))
        && first.contains("shortened from")
    {
        rest = after;
    }
    if let (Some(signals), Some((first, after))) = (&cut.signals, rest.split_once('\n'))
        && first == signals
    {
        rest = after;
    }
    rest
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
    // A numbered line (`   251\tsome text`, as read_file shows a file) is
    // content like the first one, never a note.
    let numbered = |s: &str| {
        let t = s.trim_start();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && t[digits..].starts_with('\t')
    };
    if l.trim().is_empty()
        || chars(l) + 1 > room / 5
        || shape(l) == shape(row)
        || (numbered(l) && numbered(row))
    {
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
    // An odd section of a configuration (`[delivery.default]`) brings its
    // lines – the values that make it odd.
    let heads = |i: usize| {
        let l = body[i].trim();
        l.starts_with('[') && l.ends_with(']')
    };
    let mut order: Vec<usize> = Vec::new();
    for &i in &odd {
        order.push(i);
        if heads(i) {
            order.extend(
                (i + 1..body.len())
                    .take(6)
                    .take_while(|&j| !body[j].trim().is_empty() && !heads(j)),
            );
        }
    }
    for i in order.into_iter().chain(0..body.len()) {
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
    // How often each kind of line repeats (filler or not).
    let mut common: HashMap<u64, usize> = HashMap::new();
    for l in &all {
        *common.entry(pattern_of(l)).or_default() += 1;
    }
    // A small room: the findings before the counts and the long end.
    let small = room < 300;
    let mut out = String::new();
    if let Some(signals) = signals.filter(|_| !small) {
        out.push_str(signals);
        out.push('\n');
    }
    // Its first line – the command, what it was about – always, where room
    // allows (a finding there has no error word to be found by).
    // In a small room only when it is short (a third of the room).
    let first = all
        .iter()
        .position(|l| !is_marker(l))
        .filter(|&f| f + 1 < all.len())
        .filter(|&f| !small || chars(plain(all[f])) <= room / 3);
    // Shown already: the first line – and, when it names a failure, the
    // detail after it.
    let mut shown: Vec<usize> = Vec::new();
    if let Some(f) = first {
        out.push_str(head(plain(all[f]), (room / 6).clamp(40, 200)));
        out.push('\n');
        shown.push(f);
        if is_key_line(all[f])
            && let Some(next) = all.get(f + 1).filter(|l| !is_marker(l))
            && !is_key_line(next)
            && f + 1 < all.len() - 1
        {
            out.push_str(&format!("  {}\n", squeeze(plain(next), 200)));
            shown.push(f + 1);
            // And the other side of a diagnosis (`Expected:` / `Received:`).
            if is_diagnosis(next)
                && let Some(more) = all
                    .get(f + 2)
                    .filter(|l| is_diagnosis(l) && !is_key_line(l))
                && f + 2 < all.len() - 1
            {
                out.push_str(&format!("  {}\n", squeeze(plain(more), 200)));
                shown.push(f + 2);
            }
        }
    }
    // The end: half the room – in a small one only the last line (the
    // exit code, the verdict).
    let last_line = all[all.len() - 1];
    let end_room = if small {
        chars(last_line) + 1
    } else {
        room / 2
    };
    // An end of look-alike lines (`cleanup shard=0717 removed` …) says
    // little: its last lines only, the room goes to the findings.
    let end_room = {
        let mut size = 0;
        let mut alike: HashMap<u64, usize> = HashMap::new();
        let mut n = 0;
        for l in all.iter().rev() {
            size += chars(l) + 1;
            if size > end_room {
                break;
            }
            *alike.entry(pattern_of(l)).or_default() += 1;
            n += 1;
        }
        let most = alike.values().copied().max().unwrap_or(0);
        if !small && n >= 3 && most * 10 >= n * 7 {
            all.iter()
                .rev()
                .take(3)
                .map(|l| chars(l) + 1)
                .sum::<usize>()
                + 16
        } else {
            end_room
        }
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
    let mut distinct: std::collections::HashSet<&str> =
        shown.iter().map(|&i| plain(all[i])).collect();
    let keys: Vec<usize> = (0..tail_from)
        .filter(|i| !shown.contains(i) && !is_marker(all[*i]) && is_key_line(all[*i]))
        .filter(|&i| distinct.insert(plain(all[i])))
        .collect();
    let mut skip = keys.clone();
    skip.extend(shown.iter().copied());
    let rare = off_pattern(&all[..tail_from], &skip);
    let keys_set: std::collections::HashSet<usize> = keys.iter().copied().collect();
    // From both ends of the log, then those in between; then the odd ones.
    // One of each kind of finding first (a schema error among two hundred
    // missing values), then from both ends of the log, then the odd lines.
    let kind = |i: usize| -> String {
        plain(all[i])
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .take(40)
            .collect()
    };
    let mut kinds = std::collections::HashSet::new();
    let mut order: Vec<usize> = keys
        .iter()
        .copied()
        .filter(|&i| kinds.insert(kind(i)))
        .collect();
    let mut in_order: std::collections::HashSet<usize> = order.iter().copied().collect();
    let rest: Vec<usize> = keys
        .iter()
        .copied()
        .filter(|i| !in_order.contains(i))
        .collect();
    in_order.extend(rest.iter().copied());
    let (mut a, mut b) = (0usize, rest.len());
    while a < b {
        order.push(rest[a]);
        a += 1;
        if a < b {
            b -= 1;
            order.push(rest[b]);
        }
    }
    // Odd lines right after a finding first (its `Expected:`), then the
    // others.
    let mut after_key = vec![false; all.len()];
    for &k in &keys {
        for slot in after_key
            .iter_mut()
            .take((k + 7).min(all.len()))
            .skip(k + 1)
        {
            *slot = true;
        }
    }
    let near = |i: usize| after_key[i];
    // A run's summary before the other odd lines (`Tests run: 84, …,
    // Skipped: 2` in the middle of a build).
    static SUMMARY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(tests? run|passed|skipped|warnings?|deselected|xfailed)\b.*\d|\d+ (tests?|passed|skipped)\b")
            .expect("valid")
    });
    // A warning in a run that went through: what to look at.
    let warnings: Vec<usize> = (0..tail_from)
        .filter(|&i| {
            WARNING.is_match(all[i])
                && !keys_set.contains(&i)
                && !shown.contains(&i)
                && !in_order.contains(&i)
                && common.get(&pattern_of(all[i])).copied().unwrap_or(0) < 3
        })
        .collect();
    in_order.extend(warnings.iter().copied());
    order.extend(warnings);
    let summaries: Vec<usize> = (0..tail_from)
        .filter(|&i| {
            SUMMARY.is_match(all[i])
                && !keys_set.contains(&i)
                && !shown.contains(&i)
                && !in_order.contains(&i)
                && common.get(&pattern_of(all[i])).copied().unwrap_or(0) < 3
        })
        .collect();
    in_order.extend(summaries.iter().copied());
    order.extend(summaries);
    order.extend(rare.iter().copied().filter(|&i| near(i)));
    order.extend(rare.iter().copied().filter(|&i| !near(i)));
    let room_for_findings = room.saturating_sub(chars(&out) + end_room.min(room / 2) + 1);
    // A finding's line: longer where there is room (a long response with
    // its reason in the middle).
    let line_cap = (room_for_findings / 2).clamp(200, 400);
    let mut picked: std::collections::BTreeMap<usize, String> = Default::default();
    let mut used = 0;
    let mut left_out = 0;
    let total_keys = keys_set.len();
    let mut seen_keys = 0;
    for i in order {
        // Full: the rest of the findings are counted, not tried one by one
        // (a log of thousands of failures).
        if room_for_findings.saturating_sub(used) < 24 {
            left_out += total_keys.saturating_sub(seen_keys);
            break;
        }
        seen_keys += usize::from(keys_set.contains(&i));
        // The detail after a finding (not one of the findings itself, nor
        // a line like many others): its next line – and the one after when
        // both are short (`left:` and `right:`, `Expected:` and `Received:`).
        let detail = |j: usize| {
            j < tail_from
                && !keys_set.contains(&j)
                && !picked.contains_key(&j)
                && !shown.contains(&j)
                && !distinct_shown(&shown, &all, j)
                && !is_marker(all[j])
                && (common.get(&pattern_of(all[j])).copied().unwrap_or(0) < 3
                    || is_diagnosis(all[j]))
        };
        // A finding with what explains it: the line before it when that
        // one is its own (`Applying 20260922_unique_ref`), the line after
        // it, and the diagnosis a few lines on (`^^^ expected …, found …`,
        // `E   assert 6 == 7`).
        let mut entry = Vec::new();
        if keys_set.contains(&i)
            && i > 0
            && !shown.contains(&(i - 1))
            && !keys_set.contains(&(i - 1))
            && !is_marker(all[i - 1])
            && common.get(&pattern_of(all[i - 1])).copied().unwrap_or(0) < 3
        {
            entry.push((i - 1, format!("  {}", squeeze(plain(all[i - 1]), 200))));
        }
        entry.push((i, format!("! {}", squeeze(plain(all[i]), line_cap))));
        let mut more = Vec::new();
        if detail(i + 1) {
            more.push((i + 1, format!("  {}", squeeze(plain(all[i + 1]), 200))));
            if detail(i + 2) && chars(plain(all[i + 1])) <= 100 && chars(plain(all[i + 2])) <= 100 {
                more.push((i + 2, format!("  {}", plain(all[i + 2]))));
            }
        }
        for (j, line) in all
            .iter()
            .enumerate()
            .take((i + 11).min(tail_from))
            .skip(i + 1)
        {
            if more.len() >= 5 {
                break;
            }
            if is_diagnosis(line) && detail(j) && !more.iter().any(|(k, _)| *k == j) {
                more.push((j, format!("  {}", squeeze(plain(line), 200))));
            }
        }
        // Diagnoses first: they are what is dropped last.
        more.sort_by_key(|(j, _)| (!is_diagnosis(all[*j]), *j));
        entry.extend(more);
        if picked.contains_key(&i) {
            continue;
        }
        // What fits: with its detail, with less of it, alone.
        let cost_of = |e: &[(usize, String)]| -> usize {
            e.iter()
                .filter(|(j, _)| !picked.contains_key(j))
                .map(|(_, l)| chars(l) + 1)
                .sum()
        };
        while entry.len() > 1 && used + cost_of(&entry) > room_for_findings {
            entry.pop();
        }
        let cost = cost_of(&entry);
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
        let t = compress::compress(
            text,
            false,
            Options {
                max_summary_len: tail_room,
                max_tail_lines: usize::MAX,
                overflow: Overflow::Tail,
            },
        )
        .summary;
        // Cut from its start: the end begins with the next whole line.
        match t.strip_prefix("… [truncated]\n") {
            Some(rest) => match rest.split_once('\n') {
                Some((_, whole)) if !whole.is_empty() => format!("… [truncated]\n{whole}"),
                _ => t,
            },
            None => t,
        }
    };
    out.push_str(&tail);
    // Never over the room, whatever the parts added up to.
    if chars(&out) > room {
        return last(&out, room).to_string();
    }
    out
}

/// Whether line `i` reads as one shown already (an earlier cut's copy).
fn distinct_shown(shown: &[usize], all: &[&str], i: usize) -> bool {
    shown.iter().any(|&s| plain(all[s]) == plain(all[i]))
}

/// A long line in `cap` characters: its start (what kind of line) and its
/// end (where a diagnosis usually ends: the reason, the field).
fn squeeze(line: &str, cap: usize) -> String {
    let n = chars(line);
    if n <= cap {
        return line.to_string();
    }
    let front = cap / 3;
    let back = cap - front - 3;
    // A finding word between start and end (`"error":"RATE_LIMIT"` in a
    // long response): the start, the words around it, the end.
    // The strongest one: shouted (`ERROR`) over spoken, never one that
    // says there is none (`error: none`).
    let middle = KEY_WORD
        .find_iter(line)
        .filter_map(|m| {
            let at = line[..m.start()].chars().count();
            (at > front && at + 20 < n - back / 2).then_some((m, at))
        })
        .map(|(m, at)| {
            let word = m.as_str();
            let after: String = line[m.end()..]
                .chars()
                .take(12)
                .collect::<String>()
                .to_lowercase();
            let none = ["none", "null", "0", "false", "ok", "no"].iter().any(|x| {
                after
                    .trim_start_matches([':', '=', ' ', '"'])
                    .starts_with(x)
            });
            let loud = word
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(char::is_uppercase);
            (i32::from(loud) * 2 - i32::from(none) * 3, at)
        })
        .max_by_key(|&(score, at)| (score, std::cmp::Reverse(at)))
        .map(|(_, at)| at);
    if let Some(at) = middle {
        // Most of the room to what is around the finding word.
        let (f, b) = (cap / 6, cap / 6);
        let w = cap - f - b - 6;
        let from = at.saturating_sub(w / 6);
        let window: String = line.chars().skip(from).take(w).collect();
        return format!("{} … {window} … {}", head(line, f), last(line, b));
    }
    format!("{} … {}", head(line, front), last(line, back))
}

/// A line as found, without what an earlier cut put before it.
fn plain(line: &str) -> &str {
    line.trim().trim_start_matches("! ")
}

/// A line an earlier cut wrote (`… [truncated]`, `… [omitted 40 lines]`,
/// `! … 3 more such lines`, `…`) – not content of the output.
fn is_marker(line: &str) -> bool {
    let l = plain(line);
    l == "…" || l.starts_with("… [") || l.starts_with("… ") && l.ends_with("more such lines")
}

/// A line of a diagnosis (`Expected: 10`, `right: Err(…)`, pytest's `E`):
/// detail, however often its like appears.
fn is_diagnosis(line: &str) -> bool {
    static DIAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^\s*(expected|received|actual|got|want(ed)?|left|right|assert(ion)?|caused by|E\s{2,}|-->|note:)|\bexpected\b.*\bfound\b|^\s*\d*\s*\|\s*\^|^\S+:\d+:\s*\w*error")
            .expect("valid")
    });
    DIAG.is_match(plain(line))
}

/// The numbers of a line, as they stand (scanned, no regex: a large log
/// has tens of thousands of lines).
fn numbers_in(line: &str) -> Vec<&str> {
    let l = plain(line);
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in l.char_indices() {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push(&l[st..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(&l[st..]);
    }
    out
}

/// A line with its digits left out: lines alike but for a counter share it.
fn pattern_of(l: &str) -> u64 {
    // FNV-1a over the bytes but digits: no string made per line.
    plain(l)
        .bytes()
        .filter(|b| !b.is_ascii_digit())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// In a log of repeated lines (`check_0001 ... ok` a thousand times), the
/// lines that fit none of its patterns – a finding without an error word
/// (`required=17 observed=3 decision=hold`). Patterns: the line with its
/// digits left out. Nothing in a log without repetition.
fn off_pattern(lines: &[&str], skip: &[usize]) -> Vec<usize> {
    let skip: std::collections::HashSet<&usize> = skip.iter().collect();
    let pattern = pattern_of;
    let patterns: Vec<u64> = lines.iter().map(|l| pattern(l)).collect();
    let mut count: HashMap<u64, usize> = HashMap::new();
    for &p in &patterns {
        *count.entry(p).or_default() += 1;
    }
    // Repetitive enough to tell the odd ones out: most lines repeat a
    // pattern (one, or the fields of a block repeated – `carrier = …`,
    // `zone = …` sixty times).
    let covered: usize = count.values().filter(|&&n| n >= 5).sum();
    if lines.len() < 20 || covered * 3 < lines.len() * 2 {
        return Vec::new();
    }
    let mut odd: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !skip.contains(i) && !is_marker(l) && count[&patterns[*i]] == 1)
        .map(|(i, _)| i)
        .collect();
    // Alike but for a value most of them share (`timeout_ms = 0` among
    // `timeout_ms = 30000`): a counter differs in every line, a value in few.
    let numbers = numbers_in;
    let mut groups: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, &p) in patterns.iter().enumerate() {
        groups.entry(p).or_default().push(i);
    }
    for members in groups.values().filter(|m| m.len() >= 20) {
        let values: Vec<Vec<&str>> = members.iter().map(|&i| numbers(lines[i])).collect();
        let width = values.iter().map(Vec::len).min().unwrap_or(0);
        for k in 0..width {
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for v in &values {
                *seen.entry(v[k]).or_default() += 1;
            }
            let Some((&top, &n)) = seen.iter().max_by_key(|(_, n)| **n) else {
                continue;
            };
            if n * 10 >= members.len() * 8 && n < members.len() {
                odd.extend(
                    members
                        .iter()
                        .zip(&values)
                        .filter(|(i, v)| v[k] != top && !skip.contains(i))
                        .map(|(&i, _)| i),
                );
            }
        }
    }
    odd.sort_unstable();
    odd.dedup();
    odd
}

/// A line that names an error or a failure (the port's markers, and the
/// failure words of the log summary).
fn is_key_line(line: &str) -> bool {
    KEY_WORD.is_match(line)
}

/// A warning (not a failure).
static WARNING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?-u:\b)(warning|warn|deprecated|warnung|veraltet)(?-u:\b)")
        .expect("valid")
});

/// The words of a finding.
static KEY_WORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        // ASCII word boundaries: the words are ASCII, and Unicode ones
        // would make the scan of a large log ten times slower.
        r"(?i)error:|failed:|traceback \(most recent call last\)|assertionerror|exception:|http/[0-9.]+ [45][0-9][0-9]|(?-u:\b)(panicked|error|exception|fail|failed|rejected|denied|aborted|fehler|fehlgeschlagen|abgelehnt)(?-u:\b)",
    )
    .expect("valid")
});

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
            "lines": {"type": "integer", "description": "How many lines (default 150, at most 400)"},
            "from_char": {"type": "integer", "description": "In a very long line (with from_line): the character to start at"}
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
    #[serde(default)]
    from_char: Option<usize>,
}

/// The first line to read (0-based): `from_line` counts from 1, a negative
/// one from the end.
fn start_line(from_line: Option<i64>, total: usize) -> usize {
    match from_line {
        Some(n) if n < 0 => total.saturating_sub(n.unsigned_abs() as usize),
        Some(n) => (n.max(1) as usize - 1).min(total),
        None => 0,
    }
}

/// Runs `read_result`: (text, is_error).
pub fn read(store: &ResultStore, args: &Value) -> (String, bool) {
    let a: ReadArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return (format!("invalid arguments: {e}"), true),
    };
    let id = a.id.trim().trim_start_matches('"').trim_end_matches('"');
    let Some(kept) = store.get(head(id, 20)) else {
        return (
            format!(
                "there is no result {} in this conversation (or it is no longer kept)",
                head(id, 20)
            ),
            true,
        );
    };
    let lines: Vec<&str> = kept.text.lines().collect();
    let total = lines.len();
    let status = if kept.error { " · FAILED" } else { "" };
    let label = format!("[{} · {}{status} · {total} lines]", kept.id, kept.tool);
    // Room for the rows: the whole answer, its head and a closing note
    // included, stays within READ_MAX_CHARS.
    let room = READ_MAX_CHARS - 300;
    let mut out = String::new();
    if let Some(q) = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        // Searched as given – a query too long is refused, never cut.
        if chars(q) > MAX_QUERY_CHARS {
            return (
                format!("the query is too long – at most {MAX_QUERY_CHARS} characters"),
                true,
            );
        }
        let find = regex::RegexBuilder::new(&regex::escape(q))
            .case_insensitive(true)
            .build()
            .expect("an escaped query");
        // From a line on (paging through many hits; negative: from the
        // end), at most `lines` hits.
        let from = start_line(a.from_line, total);
        let most = a
            .lines
            .map_or(READ_MAX_LINES, |n| n.clamp(1, READ_MAX_LINES));
        let hits: Vec<(usize, &str)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| find.is_match(l))
            .map(|(i, l)| (i, *l))
            .collect();
        out.push_str(&format!(
            "{label} {} lines contain {:?}\n",
            hits.len(),
            head(q, 60)
        ));
        for (shown, (i, l)) in hits.iter().filter(|(i, _)| *i >= from).enumerate() {
            let row = format!("{:>6}\t{}\n", i + 1, around(l, Some(&find)));
            if shown >= most || chars(&out) + chars(&row) > room {
                out.push_str(&format!(
                    "… more – query with from_line {} for the next ones\n",
                    i + 1
                ));
                break;
            }
            out.push_str(&row);
        }
        return (out, false);
    }
    let count = a.lines.unwrap_or(READ_LINES).clamp(1, READ_MAX_LINES);
    let start = start_line(a.from_line, total);
    let end = (start + count).min(total);
    out.push_str(&format!("{label} lines {}–{end}\n", start + 1));
    for (i, l) in lines[start..end].iter().enumerate() {
        let text = match a.from_char.filter(|_| i == 0) {
            // A part of a very long line, from a character on.
            Some(c) => window(l, c),
            None => around(l, None),
        };
        let row = format!("{:>6}\t{text}\n", start + i + 1);
        if chars(&out) + chars(&row) > room {
            out.push_str(&format!(
                "… (more from line {} – use from_line)\n",
                start + i + 1
            ));
            return (out, false);
        }
        out.push_str(&row);
    }
    if end < total {
        out.push_str(&format!(
            "… {} more lines (from_line {})\n",
            total - end,
            end + 1
        ));
    }
    (out, false)
}

/// A query this long at most.
const MAX_QUERY_CHARS: usize = 200;
/// A line this long is shown in part.
const LINE_CHARS: usize = 500;

/// A line as `read_result` shows it: whole when short; a long one around
/// the first match of `query` (else its start) – with what was left out said.
fn around(line: &str, query: Option<&regex::Regex>) -> String {
    let n = chars(line);
    if n <= LINE_CHARS {
        return line.to_string();
    }
    // Where the match is in the line itself (in characters).
    let at = query
        .and_then(|q| q.find(line))
        .map(|m| line[..m.start()].chars().count())
        .unwrap_or(0);
    window(line, at.saturating_sub(LINE_CHARS / 3))
}

/// [`LINE_CHARS`] characters of a line from `from` on, with what was left
/// out said (and how to read on).
fn window(line: &str, from: usize) -> String {
    let n = chars(line);
    let from = from.min(n);
    let to = (from + LINE_CHARS).min(n);
    let part: String = line.chars().skip(from).take(to - from).collect();
    format!(
        "{}{part}{} [characters {}–{to} of {n}{}]",
        if from > 0 { "…" } else { "" },
        if to < n { "…" } else { "" },
        from + 1,
        if to < n {
            format!("; from_char {to} reads on")
        } else {
            String::new()
        }
    )
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
        cut: None,
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
            cut: None,
        }
    }

    fn test_log(fail_at: usize, n: usize) -> String {
        let mut l: Vec<String> = (0..n).map(|i| format!("test_case_{i:04} ... ok")).collect();
        l[fail_at] = format!("test_case_{fail_at:04} ... FAILED: expected 12.50, got 12.49");
        l.push(format!("== {} passed, 1 failed ==", n - 1));
        l.push("[exit code 1]".into());
        l.join("\n")
    }

    // covers: FPL-02 (review 4)
    #[test]
    fn review_4_values_details_signals_and_the_budget() {
        // A value that differs (not a counter) is an odd line (44).
        let list: String = (0..500)
            .map(|i| {
                let v = if i == 251 { 0 } else { 30000 };
                format!("config_{i:04}: timeout_ms = {v}\n")
            })
            .collect();
        for budget in [600, 1000] {
            let s = shorten(&meta("grep", false, Some("r1")), &list, budget);
            assert!(s.contains("config_0251: timeout_ms = 0\n"), "{s}");
        }
        // Repeated diagnoses are details, not filler (45).
        let mut log = String::new();
        for n in 1..=5 {
            log.push_str(&format!(
                "FAILED test_{n}\nExpected: {}\nReceived: {}\n",
                n * 10,
                n * 10 + 1
            ));
        }
        log.push_str(
            &(0..300)
                .map(|i| format!("cleanup shard={i:04} removed\n"))
                .collect::<String>(),
        );
        log.push_str("exit status 1\n");
        let s = shorten(&meta("bash", true, Some("r2")), &log, 1000);
        assert!(
            s.contains("Expected: 10") && s.contains("Received: 11"),
            "{s}"
        );
        // The finding that is one, not the status that says there is none (46).
        let line = format!(
            "response: {} error: none {} ERROR code RATE_LIMIT retry_after=60 {}\n",
            "x".repeat(700),
            "y".repeat(1500),
            "z".repeat(1000)
        );
        let log = format!(
            "run checks\n{line}{}exit status 1\n",
            "cleanup ok\n".repeat(300)
        );
        let s = shorten(&meta("bash", true, Some("r3")), &log, 1000);
        assert!(s.contains("RATE_LIMIT retry_after=60"), "{s}");
        // Never over the budget (48).
        let mut grep: String = (0..100)
            .map(|i| format!("src/workers/job_{i:03}.rs:48: timeout_ms = 30000\n"))
            .collect();
        grep.push_str("src/router.rs:214: timeout_ms = 0 // disables deadline\nsrc/router.rs:215: retries = 0\n");
        assert!(chars(&shorten(&meta("grep", false, Some("r4")), &grep, 150)) <= 150);
    }

    // covers: FPL-02 (round 3 of the held-out cases)
    #[test]
    fn details_odd_lines_and_long_reasons_survive() {
        // An assertion's two sides, after a run of passing tests.
        let mut log: String = (0..300).map(|i| format!("test t{i} ... ok\n")).collect();
        log.push_str("test leases::expired ... FAILED\n  left: Ok(())\n right: Err(Expired)\n");
        log.push_str(
            &(0..200)
                .map(|i| format!("test u{i} ... ok\n"))
                .collect::<String>(),
        );
        log.push_str("test result: FAILED. 499 passed; 1 failed\n");
        let s = shorten(&meta("bash", true, Some("r1")), &log, 600);
        assert!(
            s.contains("left: Ok(())") && s.contains("right: Err(Expired)"),
            "{s}"
        );
        // The odd ones of a list of look-alikes, even in a small room.
        let mut grep: String = (0..400)
            .map(|i| format!("src/workers/job_{i:03}.rs:48: timeout_ms = 30000\n"))
            .collect();
        grep.push_str("src/router.rs:214: timeout_ms = 0\nsrc/router.rs:215: retries = 0\n");
        let s = shorten(&meta("grep", false, Some("r2")), &grep, 150);
        assert!(
            s.contains("timeout_ms = 0\n") && s.contains("retries = 0"),
            "{s}"
        );
        // A reason in the middle of a very long line.
        let line = format!(
            "HTTP 429 body={{\"pad\":\"{}\",\"error\":\"RATE_LIMIT\",\"detail\":\"batch rejected\",\"tail\":\"{}\"}}\n",
            "x".repeat(5000),
            "y".repeat(5000)
        );
        let log = format!("$ push\n{line}{}exit 1\n", "INFO released\n".repeat(300));
        let s = shorten(&meta("bash", true, Some("r3")), &log, 650);
        assert!(
            s.contains("\"error\":\"RATE_LIMIT\"") && s.contains("batch rejected"),
            "{s}"
        );
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
        // The budget: 50 ms per cut in a release build (four cuts here);
        // a debug build is many times slower.
        let ms = started.elapsed().as_millis();
        let bound = if cfg!(debug_assertions) { 15_000 } else { 200 };
        assert!(ms < bound, "{ms} ms");
    }

    #[test]
    fn a_log_cut_twice_shows_no_marker_or_broken_line() {
        let mut log: Vec<String> = (0..3600).map(|i| format!("scan {i:04} cached")).collect();
        log[641] = "FAIL invoice_a17: expected=72.0 actual=70".into();
        log[902] = "FAIL invoice_b09: expected=40.0 actual=30".into();
        let log = log.join("\n");
        let m = meta("bash", true, Some("r1"));
        let once = shorten(&m, &log, 20_000);
        let cut = Meta {
            cut: Some(cut_of(&log)),
            ..m
        };
        let twice = shorten(&cut, &once, 800);
        assert!(
            twice.contains("expected=72.0 actual=70") && twice.contains("expected=40.0 actual=30"),
            "{twice}"
        );
        for line in twice.lines().skip(1) {
            let l = line.trim_start_matches("! ").trim();
            assert!(
                l == "… [truncated]"
                    || l.starts_with("(signals")
                    || l.starts_with("FAIL")
                    || l.starts_with("scan "),
                "noise: {line:?}\n{twice}"
            );
        }
        assert!(
            twice.starts_with(
                "[bash · FAILED · shortened from 3600 lines · read_result r1]\n(signals: 2 fail"
            ),
            "{twice}"
        );
    }

    // covers: FPL-02 – a tool's output cannot pass for an earlier cut
    // (review 1, finding 6): counts and signals come only from the record.
    #[test]
    fn a_label_in_a_tools_output_is_only_text() {
        let mut fake =
            String::from("[bash · shortened from 1 lines]\n(signals: 0 fail, 0 error, 999 pass)\n");
        fake.push_str(
            &(0..300)
                .map(|i| format!("step {i} ok\n"))
                .collect::<String>(),
        );
        let s = shorten(&meta("bash", false, Some("r1")), &fake, 800);
        let mut lines = s.lines();
        // Ancilo's own first lines: counted on the whole, from no text.
        assert_eq!(
            lines.next(),
            Some("[bash · shortened from 302 lines · read_result r1]")
        );
        assert_eq!(lines.next(), Some("(signals: 0 fail, 1 error, 300 pass)"));
        // The fake ones are content, below.
        assert_eq!(lines.next(), Some("[bash · shortened from 1 lines]"));
    }

    // covers: FPL-02 – a finding in a log's first line, without an error
    // word and without repetition, is kept (review 1, finding 7).
    #[test]
    fn a_logs_first_line_is_kept() {
        let mut log = String::from("pending invoice 123 must be paid\n");
        for a in b'A'..=b'Z' {
            for b in b'A'..=b'L' {
                log.push_str(&format!("routine {}{}A operation\n", a as char, b as char));
            }
        }
        log.push_str("[exit code 0]");
        for budget in [150, 200, 300, 600, 800, 2000] {
            let s = shorten(&meta("bash", false, Some("r1")), &log, budget);
            assert!(
                s.contains("pending invoice 123 must be paid"),
                "{budget}: {s}"
            );
            assert!(s.contains("[exit code 0]"), "{budget}");
        }
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

    // covers: FPL-02 – a long line is read around its match; the answer
    // stays within its budget whatever the query (review 1, findings 8, 9).
    #[test]
    fn long_lines_and_long_queries_stay_readable_and_bounded() {
        let store = ResultStore::in_memory();
        let line = format!("{}CRUCIAL finding{}", "a".repeat(600), "b".repeat(900));
        store.keep("bash", &json!({}), false, &format!("{line}\nshort\n"));
        let (found, _) = read(&store, &json!({"id": "r1", "query": "crucial"}));
        assert!(found.contains("CRUCIAL finding"), "{found}");
        assert!(found.contains("[characters "), "{found}");
        let (plain, _) = read(&store, &json!({"id": "r1"}));
        assert!(
            plain.contains("of 1515; from_char 500 reads on]"),
            "{plain}"
        );
        let (long, err) = read(&store, &json!({"id": "r1", "query": "a".repeat(15_000)}));
        assert!(err && chars(&long) <= READ_MAX_CHARS, "{}", chars(&long));
        // Searched as given: a query that is not there finds nothing (review 2, 28).
        let (none, _) = read(
            &store,
            &json!({"id": "r1", "query": format!("{}ABSENT", "a".repeat(150))}),
        );
        assert!(none.contains(" 0 lines contain"), "{none}");
        // A long line read on from a character (review 2, 8).
        let (on, _) = read(
            &store,
            &json!({"id": "r1", "from_line": 1, "from_char": 1000}),
        );
        assert!(
            on.contains("[characters 1001–1500 of 1515; from_char 1500 reads on]"),
            "{on}"
        );
        // Case mapping that changes length moves no window (review 2, 21).
        let odd = format!("{}CRUCIAL", "İ".repeat(600));
        store.keep("bash", &json!({}), false, &format!("{odd}\nx\n"));
        let (hit, _) = read(&store, &json!({"id": "r2", "query": "crucial"}));
        assert!(hit.contains("CRUCIAL"), "{hit}");
        // Paging through many hits.
        let hits: String = (0..500).map(|i| format!("hit {i}\n")).collect();
        store.keep("bash", &json!({}), false, &hits);
        let (next, _) = read(
            &store,
            &json!({"id": "r3", "query": "hit", "from_line": 401}),
        );
        assert!(
            next.contains("   401\thit 400") && !next.contains("hit 399\n"),
            "{}",
            &next[..200]
        );
        // At most `lines` hits; a negative start counts from the end
        // (review 3, 37).
        let (one, _) = read(&store, &json!({"id": "r3", "query": "hit", "lines": 1}));
        assert!(
            one.contains("     1\thit 0") && !one.contains("hit 1\n"),
            "{one}"
        );
        let (last, _) = read(
            &store,
            &json!({"id": "r3", "query": "hit", "from_line": -2}),
        );
        assert!(
            last.contains("   499\thit 498") && !last.contains("hit 497\n"),
            "{last}"
        );
        let many: String = (0..2000)
            .map(|i| format!("{i} {}\n", "x".repeat(400)))
            .collect();
        store.keep("bash", &json!({}), false, &many);
        for args in [
            json!({"id": "r2", "lines": 400}),
            json!({"id": "r2", "query": "x"}),
        ] {
            let (t, _) = read(&store, &args);
            assert!(chars(&t) <= READ_MAX_CHARS, "{}", chars(&t));
        }
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
