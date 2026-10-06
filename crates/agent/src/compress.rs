//! Shrinking long tool output – a port of Atomic Agent's compressor.
//!
//! Source: AtomicBot-ai/atomic-agent, commit
//! `02aa8a9aec6408e6e04a325932d970e70ea45174`, files
//! `src/compressor/result-compressor.ts`, `listing-caps.ts` and
//! `log-summarizer.ts` (MIT, Copyright (c) 2026 Atomic Bot – the license
//! text is in `third_party/ported/atomic-agent/LICENSE`). Their tests are
//! ported below. What Ancilo does with it (the shape of each tool's output,
//! the result's id to read it again) is in [`crate::results`].
//!
//! Deliberate differences to the original, all at the edges:
//! - Lengths count characters (Unicode scalar values); JavaScript counts
//!   UTF-16 units, so an emoji is 1 here and 2 there. Cuts never split a
//!   character.
//! - A budget smaller than the marker leaves an empty cut instead of
//!   JavaScript's `slice(0, negative)`, which kept almost everything.
//! - Whether a result failed is not a parameter of the text: `is_error`
//!   comes from the tool's structured result, never from the words in it.

use std::sync::LazyLock;

use regex::Regex;

/// Which end of an over-long summary survives (`overflow` in the original).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    /// Keep the start (the original's default).
    Head,
    /// Keep the end – for output whose last lines are the point: a
    /// command's exit, a test verdict, the final error.
    Tail,
}

/// `CompressorOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub max_summary_len: usize,
    pub max_tail_lines: usize,
    pub overflow: Overflow,
}

impl Default for Options {
    /// The original's `DEFAULTS`.
    fn default() -> Self {
        Self {
            max_summary_len: 400,
            max_tail_lines: 12,
            overflow: Overflow::Head,
        }
    }
}

/// `CompressedToolResult` without what Ancilo keeps elsewhere (tool name,
/// status and details stay in the structured result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compressed {
    pub summary: String,
    pub truncated: bool,
}

const TRUNCATED: &str = "… [truncated]";

/// `compressToolResult`: the last non-blank lines plus, for a failed
/// result, the first line that names the error.
pub fn compress(output: &str, is_error: bool, opts: Options) -> Compressed {
    let normalised = output.replace("\r\n", "\n");
    let normalised = normalised.trim_end();
    let (tail, tail_truncated) = extract_tail(normalised, opts.max_tail_lines);
    let signature = extract_signature(normalised, is_error);
    let joined = [signature.as_str(), tail.as_str()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let over = chars(&joined) > opts.max_summary_len;
    let summary = if !over {
        joined
    } else if opts.overflow == Overflow::Tail {
        keep_summary_tail(&signature, &tail, opts.max_summary_len)
    } else {
        format!(
            "{}\n{TRUNCATED}",
            head(&joined, opts.max_summary_len.saturating_sub(15))
        )
    };
    Compressed {
        summary,
        truncated: tail_truncated || over,
    }
}

/// `keepSummaryTail`: an over-long summary cut from its start; the error
/// signature stays on top of the marker.
fn keep_summary_tail(signature: &str, tail: &str, max: usize) -> String {
    let top = if signature.is_empty() {
        String::new()
    } else {
        format!("{signature}\n")
    };
    let marker = format!("{TRUNCATED}\n");
    let used = chars(&top) + chars(&marker);
    if used >= max {
        return head(&format!("{top}{marker}"), max).to_string();
    }
    format!("{top}{marker}{}", last(tail, max - used))
}

/// `extractTail`: the last `max_lines` non-blank lines.
fn extract_tail(text: &str, max_lines: usize) -> (String, bool) {
    if text.is_empty() {
        return (String::new(), false);
    }
    let lines: Vec<&str> = text.split('\n').filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max_lines {
        return (lines.join("\n"), false);
    }
    (
        format!(
            "… [omitted {} lines]\n{}",
            lines.len() - max_lines,
            lines[lines.len() - max_lines..].join("\n")
        ),
        true,
    )
}

/// `ERROR_MARKERS`.
static ERROR_MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)error:|failed:|traceback \(most recent call last\)|assertionerror|exception:")
        .expect("valid")
});

/// `extractSignature`: the first line that names the error (failed results only).
fn extract_signature(text: &str, is_error: bool) -> String {
    if !is_error {
        return String::new();
    }
    text.split('\n')
        .find(|l| ERROR_MARKERS.is_match(l))
        .map(|l| format!("key: {}", head(l.trim(), 180)))
        .unwrap_or_default()
}

/// `MAX_LISTING_SUMMARY_CHARS`.
pub const MAX_LISTING_SUMMARY_CHARS: usize = 4_000;
/// `MIN_LISTING_SUMMARY_CHARS`: never less room than the default.
pub const MIN_LISTING_SUMMARY_CHARS: usize = 400;

/// `listingResultCaps`: for an ordered listing (header, then one row per
/// record) – no tail cut (it would keep the wrong end), and room for
/// `rows` rows plus the header, held between the floor and the ceiling.
pub fn listing_caps(rows: f64, chars_per_row: f64) -> Options {
    let max_summary_len = if !rows.is_finite() || !chars_per_row.is_finite() {
        MIN_LISTING_SUMMARY_CHARS
    } else {
        let budget = (rows.floor().max(0.0) + 1.0) * chars_per_row;
        budget.clamp(
            MIN_LISTING_SUMMARY_CHARS as f64,
            MAX_LISTING_SUMMARY_CHARS as f64,
        ) as usize
    };
    Options {
        max_summary_len,
        max_tail_lines: usize::MAX,
        overflow: Overflow::Head,
    }
}

/// `LogSummary`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSummary {
    pub total_lines: usize,
    pub error_lines: usize,
    pub warning_lines: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub first_error: Option<String>,
    pub first_failure: Option<String>,
}

static ERROR_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(error|exception)\b").expect("valid"));
static WARN_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bwarn(ing)?\b").expect("valid"));
static PASS_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(PASSED|PASS|ok)\b").expect("valid"));
static FAIL_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(FAILED|FAIL)\b").expect("valid"));

/// `summariseLog`: counts of the usual test-framework signals (pytest,
/// go test, jest …), without a model.
pub fn summarise_log(output: &str) -> LogSummary {
    let mut s = LogSummary::default();
    for line in output.lines() {
        s.total_lines += 1;
        if ERROR_WORD.is_match(line) {
            s.error_lines += 1;
            s.first_error
                .get_or_insert_with(|| head(line.trim(), 200).to_string());
        } else if WARN_WORD.is_match(line) {
            s.warning_lines += 1;
        }
        if PASS_WORD.is_match(line) {
            s.pass_count += 1;
        }
        if FAIL_WORD.is_match(line) {
            s.fail_count += 1;
            s.first_failure
                .get_or_insert_with(|| head(line.trim(), 200).to_string());
        }
    }
    s
}

pub(crate) fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The first `n` characters.
pub(crate) fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The last `n` characters.
pub(crate) fn last(s: &str, n: usize) -> &str {
    let count = chars(s);
    if n >= count {
        return s;
    }
    match s.char_indices().nth(count - n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    //! Ported from `result-compressor.test.ts` and `listing-caps.test.ts`
    //! (same names and expectations), then Ancilo's own edge cases.
    use super::*;

    fn lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn opts(max_summary_len: usize, max_tail_lines: usize) -> Options {
        Options {
            max_summary_len,
            max_tail_lines,
            overflow: Overflow::Head,
        }
    }

    #[test]
    fn keeps_short_output_unchanged_except_formatting() {
        let out = compress("hello", false, Options::default());
        assert!(out.summary.contains("hello"));
        assert!(!out.truncated);
    }

    #[test]
    fn trims_long_output_to_the_configured_tail_and_marks_truncation() {
        let out = compress(&lines(400), false, opts(200, 4));
        assert!(chars(&out.summary) <= 210);
        assert!(out.summary.contains("[omitted"));
        assert!(out.truncated);
    }

    #[test]
    fn cuts_an_over_long_summary_from_the_start_when_overflow_is_tail() {
        let raw = lines(400);
        let h = compress(&raw, false, opts(200, 40));
        assert!(h.summary.contains("[omitted"));
        assert!(h.summary.ends_with("… [truncated]"));
        assert!(!h.summary.contains("line 399"));
        let t = compress(
            &raw,
            false,
            Options {
                overflow: Overflow::Tail,
                ..opts(200, 40)
            },
        );
        assert!(t.summary.starts_with("… [truncated]\n"));
        assert!(t.summary.ends_with("line 399"));
        assert!(chars(&t.summary) <= 200);
        assert!(t.truncated);
    }

    #[test]
    fn keeps_the_error_signature_above_an_overflow_tail_cut() {
        let out = compress(
            &format!("error: named it\n{}", lines(400)),
            true,
            Options {
                overflow: Overflow::Tail,
                ..opts(200, 40)
            },
        );
        assert!(
            out.summary
                .starts_with("key: error: named it\n… [truncated]\n")
        );
        assert!(out.summary.ends_with("line 399"));
        assert!(chars(&out.summary) <= 200);
    }

    #[test]
    fn extracts_the_first_error_signature_when_status_is_error() {
        let log = [
            "running tests ...",
            "collected 3 items",
            "test_auth.py::test_refresh FAILED",
            "E   AssertionError: session None after refresh",
            "====== 1 failed, 2 passed in 0.12s ======",
        ]
        .join("\n");
        let out = compress(&log, true, Options::default());
        assert!(out.summary.contains("key:"));
        assert!(out.summary.contains("AssertionError"));
    }

    #[test]
    fn head_overflow_the_default_still_keeps_the_beginning() {
        let out = compress(
            &format!("{}{}", "A".repeat(300), "Z".repeat(300)),
            false,
            opts(200, 100),
        );
        assert!(out.summary.starts_with("AAAA"));
        assert!(!out.summary.contains('Z'));
        assert!(out.summary.ends_with("… [truncated]"));
        assert!(out.truncated);
    }

    #[test]
    fn summarise_log_counts_errors_warnings_passes_and_failures() {
        let log = [
            "PASS suite/a.test.ts",
            "FAIL suite/b.test.ts",
            "  Error: boom",
            "  warn: flaky",
            "PASS suite/c.test.ts",
        ]
        .join("\n");
        let s = summarise_log(&log);
        assert!(s.pass_count >= 2);
        assert!(s.fail_count >= 1);
        assert!(s.error_lines >= 1);
        assert!(s.warning_lines >= 1);
        assert!(s.first_failure.unwrap().contains("FAIL suite/b"));
    }

    fn listing(rows: usize) -> String {
        let mut l = vec!["# header".to_string()];
        for i in (1..=rows).rev() {
            l.push(format!("row {i} {}", "x".repeat(40)));
        }
        l.join("\n")
    }

    #[test]
    fn listing_caps_budget_one_row_plus_a_header_line() {
        assert_eq!(listing_caps(20.0, 100.0).max_summary_len, 2100);
        assert_eq!(listing_caps(0.0, 500.0).max_summary_len, 500);
    }

    #[test]
    fn listing_caps_disable_line_based_tail_truncation() {
        assert_eq!(listing_caps(20.0, 100.0).max_tail_lines, usize::MAX);
    }

    #[test]
    fn listing_caps_clamp_an_outsized_budget_and_floor_a_fractional_row_count() {
        assert_eq!(MAX_LISTING_SUMMARY_CHARS, 4_000);
        assert_eq!(
            listing_caps(5000.0, 160.0).max_summary_len,
            MAX_LISTING_SUMMARY_CHARS
        );
        assert_eq!(listing_caps(-3.0, 500.0).max_summary_len, 500);
        assert_eq!(listing_caps(2.7, 500.0).max_summary_len, 1500);
    }

    #[test]
    fn listing_caps_never_budget_less_than_the_compressors_own_default() {
        assert_eq!(MIN_LISTING_SUMMARY_CHARS, 400);
        assert_eq!(listing_caps(1.0, 160.0).max_summary_len, 400);
        assert_eq!(listing_caps(0.0, 280.0).max_summary_len, 400);
        assert_eq!(listing_caps(0.0, 100.0).max_summary_len, 400);
    }

    #[test]
    fn listing_caps_fall_back_to_the_floor_rather_than_producing_a_nan_cap() {
        for caps in [
            listing_caps(f64::NAN, 160.0),
            listing_caps(20.0, f64::NAN),
            listing_caps(f64::INFINITY, 160.0),
            listing_caps(20.0, f64::INFINITY),
        ] {
            assert_eq!(caps.max_summary_len, MIN_LISTING_SUMMARY_CHARS);
        }
        assert_eq!(
            listing_caps(20.0, 0.0).max_summary_len,
            MIN_LISTING_SUMMARY_CHARS
        );
        assert_eq!(
            listing_caps(20.0, -5.0).max_summary_len,
            MIN_LISTING_SUMMARY_CHARS
        );
    }

    #[test]
    fn listing_caps_keep_the_header_and_the_newest_rows_of_an_ordered_listing() {
        let raw = listing(50);
        let defaults = compress(&raw, false, Options::default());
        assert!(!defaults.summary.contains("# header"));
        assert!(!defaults.summary.contains("row 50 "));
        assert!(chars(&defaults.summary) <= 400);
        let capped = compress(&raw, false, listing_caps(50.0, 120.0));
        assert_eq!(capped.summary.split('\n').next(), Some("# header"));
        assert!(capped.summary.contains("row 50 "));
        assert!(capped.summary.contains("row 1 "));
        assert!(!capped.summary.contains("[truncated]"));
    }

    #[test]
    fn listing_caps_are_never_worse_than_the_compressor_default_on_a_small_listing() {
        let long_row = format!(
            "38065    1        someone              0.0   0.0 {}/thing",
            "/a-long-path-segment".repeat(12)
        );
        let one_row =
            format!("PID      PPID     USER               CPU%   MEM%   COMMAND\n{long_row}");
        assert!(chars(&one_row) > 320);
        let defaults = compress(&one_row, false, Options::default());
        let capped = compress(&one_row, false, listing_caps(1.0, 160.0));
        assert!(chars(&capped.summary) >= chars(&defaults.summary));
        assert!(capped.summary.contains(&long_row));
        assert!(!capped.summary.contains("[truncated]"));
        let header_only = format!(
            "# branch: {} ({})\n(working tree clean)",
            "x".repeat(140),
            "x".repeat(140)
        );
        let clean = compress(&header_only, false, listing_caps(0.0, 280.0));
        assert!(clean.summary.ends_with("(working tree clean)"));
        assert!(!clean.summary.contains("[truncated]"));
    }

    // ---- Ancilo's own edge cases ----

    #[test]
    fn cuts_never_split_a_character_and_stay_in_budget() {
        let text = "Prüfung fehlgeschlagen: Größe ✗ 测试 🚀\n".repeat(200);
        for max in [0, 1, 14, 15, 16, 50, 400] {
            for overflow in [Overflow::Head, Overflow::Tail] {
                let out = compress(
                    &text,
                    true,
                    Options {
                        max_summary_len: max,
                        max_tail_lines: 30,
                        overflow,
                    },
                );
                // Head cuts end with "\n… [truncated]" after max-15 chars.
                assert!(chars(&out.summary) <= max.max(14), "{max} {overflow:?}");
            }
        }
    }

    #[test]
    fn an_ok_result_has_no_error_signature_even_with_error_words() {
        let out = compress(
            "error: this is a test fixture\nall good",
            false,
            Options::default(),
        );
        assert!(!out.summary.contains("key:"));
    }
}
