//! Looking at a result before keeping it (FPL-03): what a task wrote – a
//! spreadsheet, a Word document, a text – shown as it is (sheets as tables
//! with their row numbers, a document's headings, paragraphs and tables),
//! and checked by stated rules against the task:
//!
//! - **readable**: the file opens;
//! - **complete**: it is not empty, and what the task names (columns in a
//!   list, words in quotes, a total) is there;
//! - **numbers**: a total row matches the rows above it; quantity × price
//!   matches the amount.
//!
//! The checks say what they found and what they could not check; they are no
//! proof that the content is right. Formulas are never calculated: Ancilo
//! writes formula-like input as text (see `write`), and a real workbook's
//! formulas show their saved values – both are said, not changed.
//!
//! Read in the same sandboxed process as documents (`preview-document`).

use std::io::{Cursor, Read};
use std::path::Path;

use ancilo_core::{Error, Result, msg};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::extract::{Kind, MAX_BYTES, kind_of};

/// Rows shown per sheet, blocks of a document.
pub const MAX_ROWS_SHOWN: usize = 200;
pub const MAX_BLOCKS_SHOWN: usize = 400;
/// Rows a check looks at per sheet.
const MAX_ROWS_CHECKED: usize = 20_000;

/// What a file looks like inside.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Layout {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sheets: Vec<SheetView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Block>,
    /// What the preview does not show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<Limit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetView {
    pub name: String,
    /// Rows with content (their number in the sheet) – all of them as read;
    /// the first ones once [`Layout::shown`].
    pub rows: Vec<Row>,
    /// Rows with content in all.
    pub total_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Row {
    pub number: u32,
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    Table { rows: Vec<Vec<String>> },
}

/// What a preview leaves out – said, so nobody takes it for the whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Limit {
    /// Only the first rows of a sheet are shown (all are checked).
    RowsCut {
        sheet: String,
        shown: usize,
        total: usize,
    },
    /// Only the first parts of a document are shown.
    BlocksCut { shown: usize, total: usize },
    /// Fonts, colours, column widths, pictures and charts are not shown.
    NoFormatting,
    /// Formulas show their saved value; Ancilo does not calculate them.
    FormulasSaved { count: usize },
    /// This kind of file is not shown (a PDF, a picture): open it.
    NotShown,
}

/// Whether a result of this name is shown (else it is listed only).
pub fn shown(path: &str) -> bool {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "xlsx" | "xlsm" | "xls" | "ods" | "csv" | "tsv" | "docx" | "txt" | "md" | "markdown"
    )
}

/// The layout of a file in the sandboxed reader (`preview-document`).
pub fn layout_file(path: &Path) -> Result<Layout> {
    let meta = std::fs::metadata(path).map_err(|e| Error::invalid(e.to_string()))?;
    if meta.len() > MAX_BYTES {
        return Err(Error::invalid(msg(
            "doc.too_large",
            &[
                ("name", &path.display()),
                ("mb", &(MAX_BYTES / 1024 / 1024)),
            ],
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| Error::invalid(e.to_string()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    layout(&name, &bytes)
}

/// What `bytes` (a file named `name`) look like inside.
pub fn layout(name: &str, bytes: &[u8]) -> Result<Layout> {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut l = match ext.as_str() {
        "xlsx" | "xlsm" | "xls" | "ods" => sheets(name, bytes)?,
        "csv" | "tsv" => delimited(name, bytes, if ext == "tsv" { b'\t' } else { b',' })?,
        "docx" => word(name, bytes)?,
        "txt" | "md" | "markdown" => text(bytes),
        _ => Layout {
            kind: kind_of(name).unwrap_or(Kind::Text),
            sheets: Vec::new(),
            blocks: Vec::new(),
            limits: vec![Limit::NotShown],
        },
    };
    if matches!(l.kind, Kind::Spreadsheet | Kind::Word) {
        l.limits.push(Limit::NoFormatting);
    }
    Ok(l)
}

fn sheet_view(name: &str, rows: Vec<Row>) -> SheetView {
    SheetView {
        name: name.to_string(),
        total_rows: rows.len(),
        rows,
    }
}

impl Layout {
    /// What the app shows: the first rows of each sheet, the first parts of
    /// a document – with what was left out said. (Checks see everything.)
    pub fn shown(mut self) -> Layout {
        for s in &mut self.sheets {
            if s.rows.len() > MAX_ROWS_SHOWN {
                self.limits.insert(
                    0,
                    Limit::RowsCut {
                        sheet: s.name.clone(),
                        shown: MAX_ROWS_SHOWN,
                        total: s.rows.len(),
                    },
                );
                s.rows.truncate(MAX_ROWS_SHOWN);
            }
        }
        if self.blocks.len() > MAX_BLOCKS_SHOWN {
            self.limits.insert(
                0,
                Limit::BlocksCut {
                    shown: MAX_BLOCKS_SHOWN,
                    total: self.blocks.len(),
                },
            );
            self.blocks.truncate(MAX_BLOCKS_SHOWN);
        }
        self
    }
}

fn sheets(name: &str, bytes: &[u8]) -> Result<Layout> {
    use calamine::{Data, Reader, open_workbook_auto_from_rs};
    let mut book = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|e| Error::invalid(format!("cannot read the spreadsheet {name} ({e})")))?;
    let mut limits = Vec::new();
    let mut views = Vec::new();
    let mut formulas = 0usize;
    for sheet in book.sheet_names() {
        let Ok(range) = book.worksheet_range(&sheet) else {
            continue;
        };
        if let Ok(f) = book.worksheet_formula(&sheet) {
            formulas += f.used_cells().filter(|(_, _, s)| !s.is_empty()).count();
        }
        let first = range.start().map_or(0, |(r, _)| r);
        let mut all = Vec::new();
        for (i, row) in range.rows().enumerate().take(MAX_ROWS_CHECKED) {
            let mut cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    other => other.to_string(),
                })
                .collect();
            while cells.last().is_some_and(String::is_empty) {
                cells.pop();
            }
            if cells.is_empty() {
                continue;
            }
            all.push(Row {
                number: first + i as u32 + 1,
                cells,
            });
        }
        views.push(sheet_view(&sheet, all));
    }
    if formulas > 0 {
        limits.push(Limit::FormulasSaved { count: formulas });
    }
    Ok(Layout {
        kind: Kind::Spreadsheet,
        sheets: views,
        blocks: Vec::new(),
        limits,
    })
}

fn delimited(name: &str, bytes: &[u8], sep: u8) -> Result<Layout> {
    let text = String::from_utf8_lossy(bytes);
    let mut all = Vec::new();
    for (i, line) in text.lines().enumerate().take(MAX_ROWS_CHECKED) {
        if line.trim().is_empty() {
            continue;
        }
        all.push(Row {
            number: i as u32 + 1,
            cells: split_line(line, sep as char),
        });
    }
    Ok(Layout {
        kind: Kind::Spreadsheet,
        sheets: vec![sheet_view(name, all)],
        blocks: Vec::new(),
        limits: Vec::new(),
    })
}

/// One line of a CSV: fields, quotes honoured.
fn split_line(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out.iter().map(|s| s.trim().to_string()).collect()
}

fn text(bytes: &[u8]) -> Layout {
    let text = String::from_utf8_lossy(bytes);
    let mut blocks = Vec::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let hashes = para.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && para[hashes..].starts_with(' ') && !para.contains('\n') {
            blocks.push(Block::Heading {
                level: hashes as u8,
                text: para[hashes..].trim().to_string(),
            });
        } else {
            blocks.push(Block::Paragraph {
                text: para.to_string(),
            });
        }
    }
    Layout {
        kind: Kind::Text,
        sheets: Vec::new(),
        blocks,
        limits: Vec::new(),
    }
}

fn word(name: &str, bytes: &[u8]) -> Result<Layout> {
    let bad = |e: String| Error::invalid(format!("cannot read the Word file {name} ({e})"));
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(e.to_string()))?;
    let entry = zip
        .by_name("word/document.xml")
        .map_err(|e| bad(e.to_string()))?;
    let mut xml = String::new();
    entry
        .take(crate::extract::MAX_UNPACKED)
        .read_to_string(&mut xml)
        .map_err(|e| bad(e.to_string()))?;
    Ok(Layout {
        kind: Kind::Word,
        sheets: Vec::new(),
        blocks: word_blocks(&xml).map_err(bad)?,
        limits: Vec::new(),
    })
}

/// Headings (by their style), paragraphs and tables of `word/document.xml`.
fn word_blocks(xml: &str) -> std::result::Result<Vec<Block>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut style: Option<String> = None;
    let mut in_text = false;
    // Inside a table: its rows, the row and the cell being read.
    let mut depth = 0usize;
    let mut table: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => match e.local_name().into_inner() {
                "t" => in_text = true,
                "tbl" => depth += 1,
                "p" => {
                    para.clear();
                    style = None;
                }
                _ => {}
            },
            Event::Empty(e) => match e.local_name().into_inner() {
                "pStyle" => {
                    style = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.local_name().into_inner() == "val")
                        .map(|a| a.value.to_string());
                }
                "tab" => para.push('\t'),
                "br" | "cr" => para.push('\n'),
                _ => {}
            },
            Event::End(e) => match e.local_name().into_inner() {
                "t" => in_text = false,
                "p" => {
                    let text = para.trim().to_string();
                    if depth > 0 {
                        if !cell.is_empty() && !text.is_empty() {
                            cell.push('\n');
                        }
                        cell.push_str(&text);
                    } else if !text.is_empty() {
                        blocks.push(match heading_level(style.as_deref()) {
                            Some(level) => Block::Heading { level, text },
                            None => Block::Paragraph { text },
                        });
                    }
                    para.clear();
                }
                "tc" if depth == 1 => row.push(std::mem::take(&mut cell)),
                "tr" if depth == 1 => table.push(std::mem::take(&mut row)),
                "tbl" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        blocks.push(Block::Table {
                            rows: std::mem::take(&mut table),
                        });
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_text => para.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text => para.push_str(match AsRef::<str>::as_ref(&r) {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                _ => "",
            }),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(blocks)
}

fn heading_level(style: Option<&str>) -> Option<u8> {
    let s = style?.to_ascii_lowercase();
    if s == "title" {
        return Some(1);
    }
    let n = s
        .strip_prefix("heading")
        .or_else(|| s.strip_prefix("berschrift"))
        .or_else(|| s.strip_prefix("überschrift"))?;
    n.trim().parse::<u8>().ok().filter(|n| (1..=6).contains(n))
}

// ---- checks ------------------------------------------------------------------

/// Which question a check answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckArea {
    /// Does the file open?
    Readable,
    /// Is what the task asks for there?
    Complete,
    /// Do the numbers add up (where that can be checked)?
    Numbers,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CheckLevel {
    /// Checked and found right.
    Ok,
    /// Worth a look: may be fine.
    Warning,
    /// Found wrong.
    Error,
}

/// One thing a check found – said in words, with where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub area: CheckArea,
    pub level: CheckLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
}

fn find(area: CheckArea, level: CheckLevel, message: String, place: Option<String>) -> Finding {
    Finding {
        area,
        level,
        message,
        place,
    }
}

/// The findings for a file that could not be read.
pub fn unreadable(why: &str) -> Vec<Finding> {
    vec![find(
        CheckArea::Readable,
        CheckLevel::Error,
        msg("check.unreadable", &[("why", &why)]),
        None,
    )]
}

/// Checks `l` against the task (the user's words).
pub fn check(l: &Layout, task: &str) -> Vec<Finding> {
    let mut out = vec![find(
        CheckArea::Readable,
        CheckLevel::Ok,
        msg("check.readable", &[]),
        None,
    )];
    if l.limits.contains(&Limit::NotShown) {
        return out;
    }
    // Empty?
    let content: usize = match l.kind {
        Kind::Spreadsheet => l.sheets.iter().map(|s| s.rows.len()).sum(),
        _ => l
            .blocks
            .iter()
            .map(|b| match b {
                Block::Heading { text, .. } | Block::Paragraph { text } => text.trim().len(),
                Block::Table { rows } => rows.iter().flatten().map(|c| c.trim().len()).sum(),
            })
            .sum(),
    };
    if content == 0 {
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Error,
            msg("check.empty", &[]),
            None,
        ));
        return out;
    }
    if l.kind == Kind::Spreadsheet {
        for s in &l.sheets {
            if s.rows.len() == 1 {
                out.push(find(
                    CheckArea::Complete,
                    CheckLevel::Error,
                    msg("check.header_only", &[]),
                    Some(sheet_place(&s.name)),
                ));
            }
        }
    }
    // What the task names.
    let wanted = required(task);
    let have = all_text(l).to_lowercase();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|w| !have.contains(&w.to_lowercase()))
        .collect();
    if !missing.is_empty() {
        let list = missing
            .iter()
            .map(|m| format!("„{m}“"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Error,
            msg("check.missing", &[("what", &list)]),
            None,
        ));
    } else if !wanted.is_empty() {
        let list = wanted
            .iter()
            .map(|m| format!("„{m}“"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Ok,
            msg("check.has", &[("what", &list)]),
            None,
        ));
    }
    // A total the task asks for.
    let tables = tables(l);
    let wants_total = TOTAL_WORD.is_match(task);
    let has_total = tables.iter().any(|t| t.rows.iter().any(is_total_row));
    if wants_total && !has_total {
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Error,
            msg("check.no_total", &[]),
            None,
        ));
    }
    // Numbers.
    let mut checked = 0usize;
    let mut wrong = 0usize;
    for t in &tables {
        let (c, w) = check_numbers(t, &mut out);
        checked += c;
        wrong += w;
    }
    let formula_text: Vec<String> = tables
        .iter()
        .flat_map(|t| {
            t.rows.iter().flat_map(move |r| {
                r.cells
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.trim_start().starts_with('='))
                    .map(move |(i, _)| t.at(r.number, i))
            })
        })
        .collect();
    if !formula_text.is_empty() {
        out.push(find(
            CheckArea::Numbers,
            CheckLevel::Warning,
            msg("check.formula_text", &[("n", &formula_text.len())]),
            Some(
                formula_text
                    .into_iter()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ));
    }
    if checked > 0 && wrong == 0 {
        out.push(find(
            CheckArea::Numbers,
            CheckLevel::Ok,
            msg("check.numbers_ok", &[("n", &checked)]),
            None,
        ));
    } else if checked == 0 && !tables.is_empty() {
        out.push(find(
            CheckArea::Numbers,
            CheckLevel::Ok,
            msg("check.numbers_none", &[]),
            None,
        ));
    }
    out
}

/// A table to check: a sheet, or a table in a document.
struct Table {
    name: Option<String>,
    rows: Vec<Row>,
}

impl Table {
    /// Where a cell is, as a person finds it: sheet, column letter, row.
    fn at(&self, row: u32, col: usize) -> String {
        let cell = format!("{}{row}", column_letter(col));
        match &self.name {
            Some(n) => format!("{n}!{cell}"),
            None => cell,
        }
    }
}

fn sheet_place(name: &str) -> String {
    name.to_string()
}

fn column_letter(mut i: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (i % 26) as u8) as char);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s
}

fn tables(l: &Layout) -> Vec<Table> {
    let mut out: Vec<Table> = l
        .sheets
        .iter()
        .map(|s| Table {
            name: (l.sheets.len() > 1 || !s.name.is_empty()).then(|| s.name.clone()),
            rows: s.rows.clone(),
        })
        .collect();
    let mut n = 0;
    for b in &l.blocks {
        if let Block::Table { rows } = b {
            n += 1;
            out.push(Table {
                name: Some(format!("table {n}")),
                rows: rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| Row {
                        number: i as u32 + 1,
                        cells: r.clone(),
                    })
                    .collect(),
            });
        }
    }
    out
}

fn all_text(l: &Layout) -> String {
    let mut s = String::new();
    for sh in &l.sheets {
        s.push_str(&sh.name);
        s.push('\n');
        for r in &sh.rows {
            s.push_str(&r.cells.join("\t"));
            s.push('\n');
        }
    }
    for b in &l.blocks {
        match b {
            Block::Heading { text, .. } | Block::Paragraph { text } => s.push_str(text),
            Block::Table { rows } => {
                for r in rows {
                    s.push_str(&r.join("\t"));
                    s.push('\n');
                }
            }
        }
        s.push('\n');
    }
    s
}

static TOTAL_WORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(summe|gesamtsumme|gesamtbetrag|insgesamt|total|sum)\b")
        .expect("valid")
});

fn is_total_row(r: &Row) -> bool {
    r.cells
        .iter()
        .take(3)
        .any(|c| TOTAL_LABEL.is_match(c.trim()))
}

static TOTAL_LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(summe|gesamt|gesamtsumme|gesamtbetrag|insgesamt|total|grand total|sum)\b",
    )
    .expect("valid")
});

/// What the task names: columns listed after "Spalten"/"columns" (and
/// similar), and anything in quotes.
pub fn required(task: &str) -> Vec<String> {
    static LIST: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(spalten?|spaltenüberschriften|columns?|column headers|felder|fields|überschriften|abschnitten|abschnitte|sections|headings)\b\s*(?:[:\-–]|für|for|namens|named|wie|like)?\s*([^.;!?\n]+)",
        )
        .expect("valid")
    });
    static QUOTED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"[„“"»«']([^„“"»«'\n]{2,60})[“”"«»']"#).expect("valid")
    });
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        let s = s
            .trim()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        if s.chars().count() >= 2 && !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    for m in QUOTED.captures_iter(task) {
        add(&m[1]);
    }
    for m in LIST.captures_iter(task) {
        for item in m[2].split([',', '/', '&']).flat_map(|p| {
            p.split(" und ")
                .flat_map(|q| q.split(" and "))
                .flat_map(|q| q.split(" sowie "))
                .flat_map(|q| q.split(" oder "))
                .flat_map(|q| q.split(" or "))
                .map(str::to_string)
                .collect::<Vec<_>>()
        }) {
            let words: Vec<&str> = item
                .split_whitespace()
                .filter(|w| !STOP.contains(&w.to_lowercase().as_str()))
                .collect();
            // A column name is a few words; more is a sentence going on.
            if (1..=3).contains(&words.len()) && !words[0].starts_with(['„', '"']) {
                add(&words.join(" "));
            }
        }
    }
    out
}

const STOP: &[&str] = &[
    "den", "der", "die", "das", "dem", "des", "ein", "eine", "einer", "jeweils", "je", "the", "a",
    "an", "each", "for", "für", "mit", "with", "in", "im", "zu", "to",
];

/// A number as people write it: 1.234,56 · 1,234.56 · 12,5 % · € 1 200.
pub fn number(cell: &str) -> Option<f64> {
    let s: String = cell
        .trim()
        .trim_end_matches(['€', '$', '%'])
        .trim_start_matches(['€', '$'])
        .trim()
        .trim_end_matches(" EUR")
        .trim_end_matches(" USD")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}' && *c != '\'')
        .collect();
    if s.is_empty() || !s.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
    {
        return None;
    }
    let (dots, commas) = (s.matches('.').count(), s.matches(',').count());
    let normal = match (dots, commas) {
        (0, 0) => s.clone(),
        (0, 1) => s.replace(',', "."),
        (1, 0) => {
            // "1.250" with three digits after the dot: German thousands.
            let after = s.rsplit('.').next().unwrap_or_default();
            if after.len() == 3
                && s.trim_start_matches(['-', '+'])
                    .split('.')
                    .next()
                    .is_some_and(|b| !b.is_empty() && b.len() <= 3)
                && !s.starts_with("0.")
            {
                s.replace('.', "")
            } else {
                s.clone()
            }
        }
        (_, 0) => s.replace('.', ""),
        (0, _) => s.replace(',', ""),
        _ => {
            // Both: the last one is the decimal mark.
            if s.rfind(',') > s.rfind('.') {
                s.replace('.', "").replace(',', ".")
            } else {
                s.replace(',', "")
            }
        }
    };
    normal.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn show(n: f64) -> String {
    let s = format!("{n:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Total rows and quantity × price; (checked, wrong).
fn check_numbers(t: &Table, out: &mut Vec<Finding>) -> (usize, usize) {
    let (mut checked, mut wrong) = (0, 0);
    if t.rows.len() < 2 {
        return (0, 0);
    }
    let header = &t.rows[0].cells;
    let cols = t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    // Totals: each number in a total row against the numbers above it
    // (since the header, or the last total).
    let mut from = 1usize;
    for (ri, r) in t.rows.iter().enumerate().skip(1) {
        if !is_total_row(r) {
            continue;
        }
        for c in 0..cols {
            let Some(total) = r.cells.get(c).and_then(|v| number(v)) else {
                continue;
            };
            let above: Vec<f64> = t.rows[from..ri]
                .iter()
                .filter_map(|x| x.cells.get(c).and_then(|v| number(v)))
                .collect();
            if above.len() < 2 {
                continue;
            }
            let sum: f64 = above.iter().sum();
            checked += 1;
            if (sum - total).abs() > 0.011 + sum.abs() * 1e-9 {
                wrong += 1;
                let what = header
                    .get(c)
                    .filter(|h| !h.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| column_letter(c));
                out.push(find(
                    CheckArea::Numbers,
                    CheckLevel::Error,
                    msg(
                        "check.total_wrong",
                        &[
                            ("what", &what),
                            ("shown", &show(total)),
                            ("sum", &show(sum)),
                        ],
                    ),
                    Some(t.at(r.number, c)),
                ));
            }
        }
        from = ri + 1;
    }
    // Quantity × price = amount, row by row.
    let col = |re: &regex::Regex| header.iter().position(|h| re.is_match(h.trim()));
    if let (Some(q), Some(p), Some(a)) = (col(&QTY), col(&PRICE), col(&AMOUNT))
        && q != a
        && p != a
    {
        for r in t.rows.iter().skip(1).filter(|r| !is_total_row(r)) {
            let get = |i: usize| r.cells.get(i).and_then(|v| number(v));
            if let (Some(qv), Some(pv), Some(av)) = (get(q), get(p), get(a)) {
                checked += 1;
                let want = qv * pv;
                if (want - av).abs() > 0.011 {
                    wrong += 1;
                    out.push(find(
                        CheckArea::Numbers,
                        CheckLevel::Error,
                        msg(
                            "check.product_wrong",
                            &[
                                ("amount", &show(av)),
                                ("qty", &show(qv)),
                                ("price", &show(pv)),
                                ("want", &show(want)),
                            ],
                        ),
                        Some(t.at(r.number, a)),
                    ));
                }
            }
        }
    }
    (checked, wrong)
}

static QTY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(menge|anzahl|stück|stk\.?|qty|quantity|units)\b").expect("valid")
});
static PRICE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(einzelpreis|stückpreis|preis|unit price|price|ep)\b").expect("valid")
});
static AMOUNT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(betrag|gesamt|gesamtpreis|summe|total|amount|line total|gp)\b")
        .expect("valid")
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::{Sheet, docx, xlsx};

    fn sheet(rows: &[&[&str]]) -> Vec<u8> {
        xlsx(&[Sheet {
            name: "Rechnungen".into(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|c| c.to_string()).collect())
                .collect(),
        }])
        .unwrap()
    }

    fn levels(f: &[Finding], area: CheckArea) -> Vec<CheckLevel> {
        f.iter()
            .filter(|x| x.area == area)
            .map(|x| x.level)
            .collect()
    }

    #[test]
    fn a_right_table_passes_and_says_what_it_checked() {
        let x = sheet(&[
            &["Firma", "Datum", "Betrag"],
            &["Stadtwerke", "2025-01-03", "120,50"],
            &["Telekom", "2025-01-09", "39.99"],
            &["Summe", "", "160,49"],
        ]);
        let l = layout("r.xlsx", &x).unwrap();
        assert_eq!(l.sheets[0].rows[3].number, 4);
        let f = check(
            &l,
            "Erstelle eine Tabelle mit den Spalten Firma, Datum und Betrag und einer Summe.",
        );
        assert!(f.iter().all(|x| x.level == CheckLevel::Ok), "{f:#?}");
        assert!(f.iter().any(|x| x.message.contains("Firma")));
    }

    #[test]
    fn a_wrong_total_is_found_with_where() {
        let x = sheet(&[
            &["Posten", "Betrag"],
            &["A", "100"],
            &["B", "50"],
            &["Gesamt", "160"],
        ]);
        let f = check(&layout("r.xlsx", &x).unwrap(), "");
        let e: Vec<&Finding> = f.iter().filter(|x| x.level == CheckLevel::Error).collect();
        assert_eq!(e.len(), 1, "{f:#?}");
        assert_eq!(e[0].place.as_deref(), Some("Rechnungen!B4"));
        assert!(
            e[0].message.contains("160") && e[0].message.contains("150"),
            "{}",
            e[0].message
        );
    }

    #[test]
    fn quantity_times_price_is_checked_row_by_row() {
        let x = sheet(&[
            &["Artikel", "Menge", "Einzelpreis", "Gesamt"],
            &["Stift", "3", "1,20", "3,60"],
            &["Block", "2", "2,50", "4,50"],
        ]);
        let f = check(&layout("r.xlsx", &x).unwrap(), "");
        let e: Vec<&Finding> = f.iter().filter(|x| x.level == CheckLevel::Error).collect();
        assert_eq!(e.len(), 1, "{f:#?}");
        assert_eq!(e[0].place.as_deref(), Some("Rechnungen!D3"));
    }

    #[test]
    fn empty_and_missing_content_are_errors() {
        let only_header = sheet(&[&["Firma", "Betrag"]]);
        let f = check(&layout("r.xlsx", &only_header).unwrap(), "");
        assert!(
            levels(&f, CheckArea::Complete).contains(&CheckLevel::Error),
            "{f:#?}"
        );
        let doc = docx(
            Some("Kündigung"),
            "# Betreff\n\nSehr geehrte Damen und Herren",
        )
        .unwrap();
        let f = check(
            &layout("k.docx", &doc).unwrap(),
            "Schreibe eine Kündigung mit den Abschnitten Betreff, Kündigungsdatum und Unterschrift",
        );
        let missing = f
            .iter()
            .find(|x| x.level == CheckLevel::Error)
            .expect("missing");
        assert!(
            missing.message.contains("Kündigungsdatum") && missing.message.contains("Unterschrift"),
            "{}",
            missing.message
        );
        assert!(!missing.message.contains("Betreff"), "{}", missing.message);
        let empty = docx(None, "").unwrap();
        let f = check(&layout("e.docx", &empty).unwrap(), "");
        assert!(
            f.iter()
                .any(|x| x.area == CheckArea::Complete && x.level == CheckLevel::Error)
        );
    }

    #[test]
    fn formula_text_is_said_not_calculated() {
        let x = sheet(&[
            &["Posten", "Betrag"],
            &["A", "1"],
            &["B", "2"],
            &["Summe", "=SUMME(B2:B3)"],
        ]);
        let l = layout("r.xlsx", &x).unwrap();
        // Ancilo's writer keeps it text – the preview shows it as written.
        assert_eq!(l.sheets[0].rows[3].cells[1], "=SUMME(B2:B3)");
        let f = check(&l, "");
        let w = f
            .iter()
            .find(|x| x.level == CheckLevel::Warning)
            .expect("warning");
        assert_eq!(w.place.as_deref(), Some("Rechnungen!B4"));
    }

    #[test]
    fn a_document_shows_headings_paragraphs_and_tables() {
        let d = docx(
            Some("Bericht"),
            "# Ergebnis\n\nAlles gut.\n\n## Details\n\nMehr.",
        )
        .unwrap();
        let l = layout("b.docx", &d).unwrap();
        assert_eq!(
            l.blocks,
            vec![
                Block::Heading {
                    level: 1,
                    text: "Bericht".into()
                },
                Block::Heading {
                    level: 1,
                    text: "Ergebnis".into()
                },
                Block::Paragraph {
                    text: "Alles gut.".into()
                },
                Block::Heading {
                    level: 2,
                    text: "Details".into()
                },
                Block::Paragraph {
                    text: "Mehr.".into()
                },
            ]
        );
        assert!(l.limits.contains(&Limit::NoFormatting));
        let xml = r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Menge</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Preis</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Betrag</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>5</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>11</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        let blocks = word_blocks(xml).unwrap();
        assert_eq!(
            blocks,
            vec![Block::Table {
                rows: vec![
                    vec!["Menge".into(), "Preis".into(), "Betrag".into()],
                    vec!["2".into(), "5".into(), "11".into()]
                ]
            }]
        );
        let l = Layout {
            kind: Kind::Word,
            sheets: vec![],
            blocks,
            limits: vec![],
        };
        assert!(
            check(&l, "")
                .iter()
                .any(|x| x.level == CheckLevel::Error && x.place.as_deref() == Some("table 1!C2"))
        );
    }

    #[test]
    fn numbers_are_read_as_people_write_them() {
        for (s, n) in [
            ("1.234,56", 1234.56),
            ("1,234.56", 1234.56),
            ("12,5 %", 12.5),
            ("€ 1 200", 1200.0),
            ("1.250", 1250.0),
            ("0.75", 0.75),
            ("-3,2", -3.2),
            ("39.99", 39.99),
        ] {
            assert_eq!(number(s), Some(n), "{s}");
        }
        for s in ["", "abc", "2025-01-03", "=SUMME(A1)", "K-4471"] {
            assert_eq!(number(s), None, "{s}");
        }
    }

    #[test]
    fn what_a_task_names() {
        assert_eq!(
            required("Liste mit den Spalten Name, E-Mail und Telefon."),
            ["Name", "E-Mail", "Telefon"]
        );
        assert_eq!(
            required("Make a sheet with columns: Date, Amount and Category"),
            ["Date", "Amount", "Category"]
        );
        assert_eq!(
            required("Schreibe „Sehr geehrte Frau Meier“ als Anrede"),
            ["Sehr geehrte Frau Meier"]
        );
        assert!(required("Fasse die Rechnungen zusammen").is_empty());
    }

    #[test]
    fn many_rows_are_shown_in_part_and_checked_in_full() {
        let mut rows: Vec<Vec<String>> = vec![vec!["Nr".into(), "Betrag".into()]];
        for i in 1..=500 {
            rows.push(vec![i.to_string(), "2".into()]);
        }
        rows.push(vec!["Summe".into(), "999".into()]);
        let x = xlsx(&[Sheet {
            name: "Daten".into(),
            rows,
        }])
        .unwrap();
        let l = layout("d.xlsx", &x).unwrap();
        // Read whole (as the sandboxed reader hands it over) and checked whole …
        let l: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        let shown = l.clone().shown();
        // … shown in part, with that said.
        assert_eq!(
            (shown.sheets[0].rows.len(), shown.sheets[0].total_rows),
            (MAX_ROWS_SHOWN, 502)
        );
        assert!(
            shown
                .limits
                .iter()
                .any(|x| matches!(x, Limit::RowsCut { total: 502, .. }))
        );
        let f = check(&l, "");
        assert!(
            f.iter()
                .any(|x| x.level == CheckLevel::Error && x.place.as_deref() == Some("Daten!B502")),
            "{f:#?}"
        );
    }
}
