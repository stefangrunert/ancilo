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
    /// The cells that are numbers in the file itself (a spreadsheet's number
    /// cells) – read as such, not from their text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Option<f64>>,
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
    /// Only the first rows of a sheet were checked (and shown).
    NotAllChecked {
        sheet: String,
        checked: usize,
        total: usize,
    },
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
    let bytes = crate::extract::read_bounded(path).map_err(Error::invalid)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    layout(&name, &bytes)
}

/// What `bytes` (a file named `name`) look like inside.
pub fn layout(name: &str, bytes: &[u8]) -> Result<Layout> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::invalid(msg(
            "doc.too_large",
            &[("name", &name), ("mb", &(MAX_BYTES / 1024 / 1024))],
        )));
    }
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
        // Where the used range starts: its rows and columns keep their
        // places (a table from C5 is shown and named from C5).
        let (first, first_col) = range.start().unwrap_or((0, 0));
        let mut all = Vec::new();
        let mut total = 0usize;
        for (i, row) in range.rows().enumerate() {
            if row.iter().all(|c| matches!(c, Data::Empty)) {
                continue;
            }
            total += 1;
            if all.len() >= MAX_ROWS_CHECKED {
                continue;
            }
            let lead = first_col as usize;
            let mut cells: Vec<String> = vec![String::new(); lead];
            let mut values: Vec<Option<f64>> = vec![None; lead];
            for c in row {
                cells.push(match c {
                    Data::Empty => String::new(),
                    other => other.to_string(),
                });
                values.push(match c {
                    Data::Float(f) => Some(*f),
                    Data::Int(n) => Some(*n as f64),
                    _ => None,
                });
            }
            while cells.last().is_some_and(String::is_empty) {
                cells.pop();
                values.pop();
            }
            all.push(Row {
                number: first + i as u32 + 1,
                cells,
                values,
            });
        }
        if total > all.len() {
            limits.push(Limit::NotAllChecked {
                sheet: sheet.clone(),
                checked: all.len(),
                total,
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
    let records = records(&text, sep as char).ok_or_else(|| {
        Error::invalid(format!(
            "cannot read the table {name} (a quote is not closed)"
        ))
    })?;
    let total = records.len();
    let all: Vec<Row> = records
        .into_iter()
        .take(MAX_ROWS_CHECKED)
        .map(|(number, cells)| Row {
            number,
            cells,
            values: Vec::new(),
        })
        .collect();
    let mut limits = Vec::new();
    if total > all.len() {
        limits.push(Limit::NotAllChecked {
            sheet: name.to_string(),
            checked: all.len(),
            total,
        });
    }
    Ok(Layout {
        kind: Kind::Spreadsheet,
        sheets: vec![sheet_view(name, all)],
        blocks: Vec::new(),
        limits,
    })
}

/// The records of a CSV – a quoted field may hold line breaks and doubled
/// quotes – each with the line it starts on; empty lines skipped. `None`:
/// a quote is not closed.
fn records(text: &str, sep: char) -> Option<Vec<(u32, Vec<String>)>> {
    let mut out = Vec::new();
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut line = 1u32;
    let mut start = 1u32;
    let mut chars = text.chars().peekable();
    let finish = |cells: &mut Vec<String>,
                  cur: &mut String,
                  start: u32,
                  out: &mut Vec<(u32, Vec<String>)>| {
        cells.push(std::mem::take(cur).trim().to_string());
        let row = std::mem::take(cells);
        if !(row.len() == 1 && row[0].is_empty()) {
            out.push((start, row));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            '\r' if !quoted => {}
            '\n' if !quoted => {
                finish(&mut cells, &mut cur, start, &mut out);
                line += 1;
                start = line;
            }
            '\n' => {
                cur.push('\n');
                line += 1;
            }
            c if c == sep && !quoted => cells.push(std::mem::take(&mut cur).trim().to_string()),
            c => cur.push(c),
        }
    }
    if quoted {
        return None;
    }
    if !cur.is_empty() || !cells.is_empty() {
        finish(&mut cells, &mut cur, start, &mut out);
    }
    Some(out)
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
    // A Word package names its parts and where its document starts; without
    // that Word does not open it.
    if zip.by_name("[Content_Types].xml").is_err() || zip.by_name("_rels/.rels").is_err() {
        return Err(bad("it is not a complete Word document".into()));
    }
    let entry = zip
        .by_name("word/document.xml")
        .map_err(|e| bad(e.to_string()))?;
    let mut xml = String::new();
    entry
        .take(crate::extract::MAX_UNPACKED)
        .read_to_string(&mut xml)
        .map_err(|e| bad(e.to_string()))?;
    if xml.len() as u64 >= crate::extract::MAX_UNPACKED {
        return Err(bad("it is too large to be read whole".into()));
    }
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
    // Every element closed again by the end – else the file broke off – and
    // one root, the document.
    let mut open = 0i64;
    let mut roots = 0;
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        match &event {
            Event::Start(e) | Event::Empty(e) if open == 0 => {
                roots += 1;
                if roots > 1 || e.local_name().into_inner() != "document" {
                    return Err("it is not a Word document".into());
                }
                if matches!(event, Event::Start(_)) {
                    open += 1;
                }
            }
            Event::Start(_) => open += 1,
            Event::End(_) => open -= 1,
            _ => {}
        }
        match event {
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
    if open != 0 || roots == 0 {
        return Err("the document breaks off in the middle".into());
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

/// Checks `l` against the task (the user's words – all its messages, in
/// order: a later one can take back what an earlier one asked).
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
    // What was not checked is said, never passed over.
    for limit in &l.limits {
        if let Limit::NotAllChecked {
            sheet,
            checked,
            total,
        } = limit
        {
            out.push(find(
                CheckArea::Complete,
                CheckLevel::Warning,
                msg(
                    "check.partly",
                    &[("checked", checked), ("total", total), ("sheet", sheet)],
                ),
                Some(sheet_place(sheet)),
            ));
        }
    }
    // What the task asks for – where it belongs (a column in a header row,
    // a section as a heading).
    let w = wanted(task);
    let missing: Vec<&Term> = w.terms.iter().filter(|t| !present(l, t)).collect();
    let list = |ts: &[&Term]| {
        ts.iter()
            .map(|t| format!("„{}“", t.text))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !missing.is_empty() {
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Error,
            msg("check.missing", &[("what", &list(&missing))]),
            None,
        ));
    } else if !w.terms.is_empty() {
        let all: Vec<&Term> = w.terms.iter().collect();
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Ok,
            msg("check.has", &[("what", &list(&all))]),
            None,
        ));
    }
    // A total the task asks for.
    let tables = tables(l);
    // A total row without a number ("wird ergänzt") is no total; one with a
    // formula is (Ancilo keeps formulas as text – said below, not changed).
    let has_total = tables.iter().any(|t| {
        t.rows.iter().any(|r| {
            is_total_row(r)
                && (0..r.cells.len())
                    .any(|c| value(r, c).is_some() || r.cells[c].trim_start().starts_with('='))
        })
    });
    if w.total && !has_total {
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
                        values: Vec::new(),
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

// ---- what the task asks for --------------------------------------------------

/// Where a named thing belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// A column: in a table's header row.
    Column,
    /// A section: a heading (or a paragraph's start).
    Section,
    /// Words in quotes: anywhere.
    Quoted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub need: Need,
}

/// What a task asks a result to hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    pub terms: Vec<Term>,
    /// A total (a row with "Summe", "Total" …).
    pub total: bool,
}

static TOTAL_WORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(summe|gesamtsumme|gesamtbetrag|teilsumme|teilsummen|zwischensumme|zwischensummen|insgesamt|total|totals|subtotal|subtotals|sum)\b",
    )
    .expect("valid")
});
/// Words that take back what directly follows them ("ohne Datum", "keine
/// Summe", "no total", "remove the date").
const NO_BEFORE: &[&str] = &[
    "ohne", "kein", "keine", "keinen", "keiner", "no", "without", "entferne", "entfernt",
    "streiche", "lösche", "remove", "drop", "exclude", "delete", "omit",
];
/// Words that take back what directly precedes them ("Datum weg", "Datum
/// entfernen").
const NO_AFTER: &[&str] = &[
    "weg",
    "entfernen",
    "streichen",
    "löschen",
    "weglassen",
    "removed",
    "dropped",
];
/// Words that turn a taking back around ("nicht Datum entfernen", "don't
/// remove the date").
const NOT: &[&str] = &["nicht", "not", "don't", "do not", "never", "nie", "niemals"];
/// Words skipped between a "no" and what it takes back.
const FILLER: &[&str] = &[
    "den", "der", "die", "das", "dem", "des", "the", "a", "an", "ein", "eine", "einen", "spalte",
    "spalten", "column", "columns", "any",
];

fn words_of(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether the text before something takes it back: a "no" right before it
/// (articles between are fine), not itself turned around by a "not".
fn no_before(before: &str) -> bool {
    let words = words_of(before);
    let mut it = words
        .iter()
        .rev()
        .skip_while(|w| FILLER.contains(&w.as_str()));
    let Some(w) = it.next() else {
        return false;
    };
    NO_BEFORE.contains(&w.as_str()) && !it.next().is_some_and(|n| NOT.contains(&n.as_str()))
}

/// Whether the text after something takes it back ("Datum weg"), unless a
/// "not" came right before it ("nicht Datum entfernen").
fn no_after(before: &str, after: &str) -> bool {
    let next = words_of(after);
    let prev = words_of(before);
    next.first().is_some_and(|w| NO_AFTER.contains(&w.as_str()))
        && !prev.last().is_some_and(|w| NOT.contains(&w.as_str()))
}

/// Whether `name` stands in `text` as whole words.
fn has_words(text: &str, name: &str) -> bool {
    let (t, n) = (words_of(text), words_of(name));
    !n.is_empty() && t.windows(n.len()).any(|w| w == n.as_slice())
}

static LIST: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(spalten?|spaltenüberschriften|columns?|column headers|felder|fields|überschriften|abschnitten|abschnitte|sections|headings)\b\s*(?:[:\-–]|für|for|namens|named|wie|like)?\s*([^.;!?\n]+)",
    )
    .expect("valid")
});
static QUOTED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"[„“"»«']([^„“"»«'\n]{2,60})[“”"«»']"#).expect("valid")
});

/// What `task` asks for, sentence by sentence in order: a sentence that
/// says no ("ohne Datum", "keine Summe", "lass die Steuer weg", "without a
/// total") takes back what it names.
pub fn wanted(task: &str) -> Wanted {
    let mut w = Wanted::default();
    let clean = |s: &str| {
        s.trim()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string()
    };
    let add = |w: &mut Wanted, text: String, need: Need| {
        if text.chars().count() >= 2 && !w.terms.iter().any(|t| t.text.eq_ignore_ascii_case(&text))
        {
            w.terms.push(Term { text, need });
        }
    };
    for sentence in task.split(['.', ';', '!', '?', '\n']) {
        for m in QUOTED.captures_iter(sentence) {
            let text = clean(&m[1]);
            let at = m.get(0).map_or(0, |x| x.start());
            let end = m.get(0).map_or(0, |x| x.end());
            if no_before(&sentence[..at]) || no_after(&sentence[..at], &sentence[end..]) {
                w.terms.retain(|t| !t.text.eq_ignore_ascii_case(&text));
            } else {
                add(&mut w, text, Need::Quoted);
            }
        }
        for m in LIST.captures_iter(sentence) {
            let trigger = m[1].to_lowercase();
            let need = if [
                "überschriften",
                "abschnitten",
                "abschnitte",
                "sections",
                "headings",
            ]
            .contains(&trigger.as_str())
            {
                Need::Section
            } else {
                Need::Column
            };
            // "ohne Spalten …": the whole list is taken back.
            let list_negated = no_before(&sentence[..m.get(1).map_or(0, |x| x.start())]);
            for item in m[2].split([',', '/', '&']).flat_map(|p| {
                p.split(" und ")
                    .flat_map(|q| q.split(" and "))
                    .flat_map(|q| q.split(" sowie "))
                    .flat_map(|q| q.split(" oder "))
                    .flat_map(|q| q.split(" or "))
                    .flat_map(|q| q.split(" aber "))
                    .flat_map(|q| q.split(" but "))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }) {
                let all: Vec<&str> = item.split_whitespace().collect();
                let lower = |w: &&str| w.to_lowercase();
                // "…, nicht Datum", "…, ohne Steuer": this one is not wanted.
                let item_negated = all.first().is_some_and(|w| {
                    let w = lower(w);
                    NO_BEFORE.contains(&w.as_str()) || NOT.contains(&w.as_str())
                });
                let item_after = all
                    .last()
                    .is_some_and(|w| NO_AFTER.contains(&lower(w).as_str()));
                let words: Vec<&str> = all
                    .into_iter()
                    .filter(|w| {
                        let w = lower(w);
                        !STOP.contains(&w.as_str())
                            && !NO_BEFORE.contains(&w.as_str())
                            && !NO_AFTER.contains(&w.as_str())
                            && !NOT.contains(&w.as_str())
                    })
                    .collect();
                // A column name is a few words; more is a sentence going on.
                if !(1..=3).contains(&words.len()) || words[0].starts_with(['„', '"']) {
                    continue;
                }
                let text = clean(&words.join(" "));
                // A total has its own rule (a total row), never a column.
                if TOTAL_WORD.is_match(&text) {
                    continue;
                }
                if list_negated || item_negated || item_after {
                    w.terms.retain(|t| !t.text.eq_ignore_ascii_case(&text));
                } else {
                    add(&mut w, text, need);
                }
            }
        }
        // "Lass Datum weg", "remove the date", "ohne Datum": what an earlier
        // sentence asked for is taken back – only by such a taking back,
        // never by any "nicht" nearby ("Datum ist nicht optional").
        let lower = sentence.to_lowercase();
        w.terms.retain(|t| {
            let tl = t.text.to_lowercase();
            !lower.match_indices(&tl).any(|(at, _)| {
                let end = at + tl.len();
                let whole = !lower[..at]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
                    && !lower[end..]
                        .chars()
                        .next()
                        .is_some_and(char::is_alphanumeric);
                whole && (no_before(&lower[..at]) || no_after(&lower[..at], &lower[end..]))
            })
        });
        if let Some(m) = TOTAL_WORD.find(sentence) {
            w.total = !(no_before(&sentence[..m.start()])
                || no_after(&sentence[..m.start()], &sentence[m.end()..]));
        }
    }
    w
}

/// The names a task asks for (any place) – see [`wanted`].
pub fn required(task: &str) -> Vec<String> {
    wanted(task).terms.into_iter().map(|t| t.text).collect()
}

const STOP: &[&str] = &[
    "den", "der", "die", "das", "dem", "des", "ein", "eine", "einer", "jeweils", "je", "the", "a",
    "an", "each", "for", "für", "mit", "with", "in", "im", "zu", "to",
];

/// Whether `t` is where it belongs in `l`.
fn present(l: &Layout, t: &Term) -> bool {
    let want = t.text.to_lowercase();
    // Whole words: an "Update" column is no "Date" column.
    let has = |s: &str| has_words(s, &t.text);
    match t.need {
        Need::Quoted => has(&all_text(l)),
        Need::Column => {
            // A header: a sheet's first row with two cells or more (a title
            // above it does not count), a table's first row.
            l.sheets.iter().any(|s| {
                s.rows
                    .iter()
                    .take(2)
                    .find(|r| r.cells.iter().filter(|c| !c.trim().is_empty()).count() >= 2)
                    .is_some_and(|r| r.cells.iter().any(|c| has(c)))
            }) || l.blocks.iter().any(|b| match b {
                Block::Table { rows } => rows.first().is_some_and(|r| r.iter().any(|c| has(c))),
                _ => false,
            })
        }
        Need::Section => {
            l.blocks.iter().any(|b| match b {
                Block::Heading { text, .. } => has(text),
                // A paragraph that starts with it ("Betreff: …").
                Block::Paragraph { text } => text
                    .trim_start_matches(|c: char| !c.is_alphabetic())
                    .to_lowercase()
                    .starts_with(&want),
                Block::Table { .. } => false,
            }) || l.sheets.iter().any(|s| has(&s.name))
        }
    }
}

// ---- numbers -----------------------------------------------------------------

/// A number in a cell: sure, or one of two readings (`1,250`: 1.25 the
/// German way, 1250 the English way).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Num {
    Sure(f64),
    Either { de: f64, en: f64 },
}

/// Which way a column writes its numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    De,
    En,
}

impl Num {
    fn get(self, way: Way) -> f64 {
        match (self, way) {
            (Num::Sure(n), _) => n,
            (Num::Either { de, .. }, Way::De) => de,
            (Num::Either { en, .. }, Way::En) => en,
        }
    }
}

/// A number as people write it: 1.234,56 · 1,234.56 · 12,5 % · € 1 200.
pub fn read_number(cell: &str) -> Option<Num> {
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
    let parse = |t: String| t.parse::<f64>().ok().filter(|n| n.is_finite());
    let (dots, commas) = (s.matches('.').count(), s.matches(',').count());
    let digits = s.trim_start_matches(['-', '+']);
    // One mark, three digits after it, one to three before: either way.
    let three_after = |mark: char| {
        let (int, frac) = digits.split_once(mark).unwrap_or_default();
        frac.len() == 3 && (1..=3).contains(&int.len()) && int != "0"
    };
    match (dots, commas) {
        (0, 0) => parse(s).map(Num::Sure),
        (0, 1) if three_after(',') => Some(Num::Either {
            de: parse(s.replace(',', "."))?,
            en: parse(s.replace(',', ""))?,
        }),
        (1, 0) if three_after('.') => Some(Num::Either {
            de: parse(s.replace('.', ""))?,
            en: parse(s.clone())?,
        }),
        (0, 1) => parse(s.replace(',', ".")).map(Num::Sure),
        (1, 0) => parse(s).map(Num::Sure),
        (_, 0) => parse(s.replace('.', "")).map(Num::Sure),
        (0, _) => parse(s.replace(',', "")).map(Num::Sure),
        _ => {
            // Both: the last one is the decimal mark.
            if s.rfind(',') > s.rfind('.') {
                parse(s.replace('.', "").replace(',', ".")).map(Num::Sure)
            } else {
                parse(s.replace(',', "")).map(Num::Sure)
            }
        }
    }
}

/// A number the German way where a cell could be read both ways.
pub fn number(cell: &str) -> Option<f64> {
    read_number(cell).map(|n| n.get(Way::De))
}

/// The number in a cell: the file's own number, else read from its text.
fn value(r: &Row, c: usize) -> Option<Num> {
    if let Some(Some(v)) = r.values.get(c) {
        return Some(Num::Sure(*v));
    }
    r.cells.get(c).and_then(|v| read_number(v))
}

fn show(n: f64) -> String {
    let s = format!("{n:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.011 + a.abs().max(b.abs()) * 1e-9
}

/// Totals and quantity × price; (checked, wrong). Something is wrong only
/// when no reading of the numbers makes it right.
fn check_numbers(t: &Table, out: &mut Vec<Finding>) -> (usize, usize) {
    let (mut checked, mut wrong) = (0, 0);
    if t.rows.len() < 2 {
        return (0, 0);
    }
    // The header: the first row with two cells or more (a title above it
    // does not count); the data below it.
    let Some(h) = t
        .rows
        .iter()
        .take(3)
        .position(|r| r.cells.iter().filter(|c| !c.trim().is_empty()).count() >= 2)
    else {
        return (0, 0);
    };
    let header = &t.rows[h].cells;
    let first = h + 1;
    let cols = t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    // A total: a subtotal against the rows of its group; a grand total
    // against all rows (or the subtotals and what came after them); a plain
    // "Summe"/"Total" against any of these.
    let mut seg_from = first;
    let mut totals: Vec<Vec<Num>> = vec![Vec::new(); cols];
    let mut after_sub: Vec<usize> = vec![first; cols];
    // Per column: each total with what it may add up to.
    type Check = (u32, Num, Vec<Vec<Num>>);
    let mut per_col: Vec<Vec<Check>> = vec![Vec::new(); cols];
    for (ri, r) in t.rows.iter().enumerate().skip(first) {
        let Some(kind) = total_kind(r) else {
            continue;
        };
        for (c, col_totals) in totals.iter_mut().enumerate() {
            let Some(total) = value(r, c) else {
                continue;
            };
            let data = |from: usize| -> Vec<Num> {
                t.rows[from..ri]
                    .iter()
                    .filter(|x| total_kind(x).is_none())
                    .filter_map(|x| value(x, c))
                    .collect()
            };
            let seg = data(seg_from);
            let all = data(first);
            let mut subs_then = col_totals.clone();
            subs_then.extend(data(after_sub[c]));
            let candidates: Vec<Vec<Num>> = match kind {
                Total::Sub => vec![seg],
                Total::Grand => vec![all, subs_then],
                Total::Any => vec![seg, all, subs_then],
            }
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect();
            if kind == Total::Sub {
                col_totals.push(total);
                after_sub[c] = ri + 1;
            }
            if !candidates.is_empty() {
                per_col[c].push((r.number, total, candidates));
            }
        }
        seg_from = ri + 1;
    }
    // One way of reading a column's numbers for all its totals: the way
    // under which the fewest are wrong (1,250 is either 1.25 or 1250 – the
    // same in every row of one column).
    for (c, checks) in per_col.iter().enumerate() {
        if checks.is_empty() {
            continue;
        }
        let wrong_in = |way: Way| -> Vec<&Check> {
            checks
                .iter()
                .filter(|(_, total, cands)| {
                    !cands
                        .iter()
                        .any(|cand| close(cand.iter().map(|n| n.get(way)).sum(), total.get(way)))
                })
                .collect()
        };
        let (de, en) = (wrong_in(Way::De), wrong_in(Way::En));
        let (way, bad) = if en.len() < de.len() {
            (Way::En, en)
        } else {
            (Way::De, de)
        };
        checked += checks.len();
        wrong += bad.len();
        for (row, total, cands) in bad {
            let what = header
                .get(c)
                .filter(|h| !h.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| column_letter(c));
            let sum: f64 = cands[0].iter().map(|n| n.get(way)).sum();
            out.push(find(
                CheckArea::Numbers,
                CheckLevel::Error,
                msg(
                    "check.total_wrong",
                    &[
                        ("what", &what),
                        ("shown", &show(total.get(way))),
                        ("sum", &show(sum)),
                    ],
                ),
                Some(t.at(*row, c)),
            ));
        }
    }
    // Quantity × price = amount, row by row.
    let col = |re: &regex::Regex| header.iter().position(|h| re.is_match(h.trim()));
    if let (Some(q), Some(p), Some(a)) = (col(&QTY), col(&PRICE), col(&AMOUNT))
        && q != a
        && p != a
    {
        for r in t.rows.iter().skip(first).filter(|r| !is_total_row(r)) {
            if let (Some(qv), Some(pv), Some(av)) = (value(r, q), value(r, p), value(r, a)) {
                checked += 1;
                let right = [Way::De, Way::En]
                    .iter()
                    .any(|&w| close(qv.get(w) * pv.get(w), av.get(w)));
                if !right {
                    wrong += 1;
                    let (qd, pd, ad) = (qv.get(Way::De), pv.get(Way::De), av.get(Way::De));
                    out.push(find(
                        CheckArea::Numbers,
                        CheckLevel::Error,
                        msg(
                            "check.product_wrong",
                            &[
                                ("amount", &show(ad)),
                                ("qty", &show(qd)),
                                ("price", &show(pd)),
                                ("want", &show(qd * pd)),
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

/// What kind of total a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Total {
    /// Of its group ("Teilsumme", "Subtotal").
    Sub,
    /// Of everything ("Gesamtsumme", "Grand total").
    Grand,
    /// Either ("Summe", "Total").
    Any,
}

fn total_kind(r: &Row) -> Option<Total> {
    let label = r.cells.iter().find(|c| TOTAL_LABEL.is_match(c.trim()))?;
    let l = label.trim().to_lowercase();
    Some(
        if ["teilsumme", "zwischensumme", "subtotal", "sub-total"]
            .iter()
            .any(|w| l.starts_with(w))
        {
            Total::Sub
        } else if [
            "gesamtsumme",
            "endsumme",
            "grand total",
            "gesamtbetrag",
            "insgesamt",
        ]
        .iter()
        .any(|w| l.starts_with(w))
        {
            Total::Grand
        } else {
            Total::Any
        },
    )
}

/// A row that is a total: its label says so – only the word (and a
/// "netto", a currency, a colon), not a row that merely starts with it
/// ("Total service").
fn is_total_row(r: &Row) -> bool {
    r.cells.iter().any(|c| TOTAL_LABEL.is_match(c.trim()))
}

static TOTAL_LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(summe|gesamt|gesamtsumme|gesamtbetrag|insgesamt|zwischensumme|teilsumme|endsumme|total|subtotal|sub-total|grand total|sum)(\s+(netto|brutto|eur|€|usd|\$|\(.*\)))?\s*:?$",
    )
    .expect("valid")
});

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

    fn csv(text: &str, task: &str) -> Vec<Finding> {
        check(&layout("t.csv", text.as_bytes()).unwrap(), task)
    }

    fn errors(f: &[Finding]) -> Vec<&Finding> {
        f.iter().filter(|x| x.level == CheckLevel::Error).collect()
    }

    // The cases of review 1 (Codex), findings 10–16.
    #[test]
    fn review_1_rows_past_the_check_are_said() {
        let mut t = String::from("Item,Amount\n");
        for i in 0..19_999 {
            t.push_str(&format!("I{i},1\n"));
        }
        t.push_str("Total,999999\nExtra,1\n");
        let f = csv(&t, "");
        assert!(
            f.iter()
                .any(|x| x.level == CheckLevel::Warning && x.message.contains("20000 of 20002")),
            "{f:#?}"
        );
    }

    #[test]
    fn review_1_numbers_that_read_two_ways_are_not_called_wrong() {
        assert!(
            errors(&csv(
                "Item,Amount\nA,\"1,250\"\nB,\"2,500\"\nTotal,3750",
                ""
            ))
            .is_empty()
        );
        assert!(errors(&csv("Posten;Betrag\nA;1,25\nB;2,50\nSumme;3,75", "")).is_empty());
        // A number cell of the file is read as a number, not from its text.
        let x = sheet(&[
            &["Artikel", "Menge", "Preis", "Betrag"],
            &["A", "2", "1.234", "2.47"],
        ]);
        assert!(errors(&check(&layout("r.xlsx", &x).unwrap(), "")).is_empty());
        // Wrong in every reading: wrong.
        assert_eq!(
            errors(&csv(
                "Item,Amount\nA,\"1,250\"\nB,\"2,500\"\nTotal,9999",
                ""
            ))
            .len(),
            1
        );
    }

    #[test]
    fn review_1_totals_of_one_item_of_groups_and_only_real_labels() {
        assert_eq!(errors(&csv("Item,Amount\nA,10\nTotal,999", "")).len(), 1);
        let groups = "Item,Amount\nA,10\nB,20\nTotal,30\nC,40\nD,50\nTotal,90\nGrand total,999";
        let e = csv(groups, "");
        assert_eq!(errors(&e).len(), 1, "{e:#?}");
        assert_eq!(errors(&e)[0].place.as_deref(), Some("t.csv!B8"));
        assert!(
            errors(&csv(
                "Item,Amount\nA,10\nB,20\nTotal,30\nC,40\nD,50\nTotal,90\nGrand total,120",
                ""
            ))
            .is_empty()
        );
        // "Total service" is a row like any other.
        assert!(errors(&csv("Item,Amount\nTotal service,10\nB,20\nTotal,30", "")).is_empty());
    }

    #[test]
    fn review_1_a_table_away_from_a1_keeps_its_cells() {
        let mut book = rust_xlsxwriter::Workbook::new();
        let ws = book.add_worksheet();
        ws.write_string(4, 2, "Item").unwrap();
        ws.write_string(4, 3, "Amount").unwrap();
        ws.write_string(5, 2, "A").unwrap();
        ws.write_number(5, 3, 10.0).unwrap();
        ws.write_string(6, 2, "B").unwrap();
        ws.write_number(6, 3, 20.0).unwrap();
        ws.write_string(7, 2, "Total").unwrap();
        ws.write_number(7, 3, 99.0).unwrap();
        let x = book.save_to_buffer().unwrap();
        let l = layout("o.xlsx", &x).unwrap();
        assert_eq!(l.sheets[0].rows[0].number, 5);
        assert_eq!(l.sheets[0].rows[0].cells[2], "Item");
        let e = check(&l, "");
        assert_eq!(errors(&e)[0].place.as_deref(), Some("Sheet1!D8"), "{e:#?}");
    }

    #[test]
    fn review_1_csv_fields_over_lines_and_broken_quotes() {
        let e = csv("Item,Amount\n\"multi\nline\",10\nB,20\nTotal,30", "");
        assert!(e.iter().any(|x| x.message.contains("1 total(s)")), "{e:#?}");
        assert!(layout("b.csv", b"Item,Amount\n\"open,10\nB,20").is_err());
    }

    #[test]
    fn review_1_what_the_task_asks_is_looked_for_where_it_belongs() {
        // Named columns in a cell are no columns.
        let e = csv(
            "Item,Comment\nA,Price Quantity Amount",
            "a sheet with the columns Price, Quantity, Amount",
        );
        assert_eq!(errors(&e).len(), 1, "{e:#?}");
        // What the task rules out is not asked for.
        assert!(
            errors(&csv(
                "Item,Amount\nA,10",
                "Keine Summe. Ohne Spalten Datum und Steuer."
            ))
            .is_empty()
        );
        let w = wanted("Mach eine Tabelle mit den Spalten Name und Datum.\nLass Datum weg.");
        assert_eq!(
            w.terms.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["Name"]
        );
        assert!(!wanted("No total, please.").total && wanted("Add a total at the end.").total);
    }

    #[test]
    fn review_2_one_reading_of_numbers_per_column() {
        // 3.75 needs 1,250 = 1.25; 8250 needs 1,250 = 1250: not both.
        let e = csv(
            "Item,Amount\nA,\"1,250\"\nB,\"2,500\"\nSubtotal,3.75\nC,\"4,500\"\nGrand total,8250",
            "",
        );
        assert_eq!(errors(&e).len(), 1, "{e:#?}");
        // One reading for all: fine.
        let e = csv(
            "Item,Amount\nA,\"1,250\"\nB,\"2,500\"\nSubtotal,3750\nC,\"4,500\"\nGrand total,8250",
            "",
        );
        assert!(errors(&e).is_empty(), "{e:#?}");
    }

    #[test]
    fn review_2_a_total_label_anywhere_in_its_row() {
        let e = csv(
            "Nr,Datum,Notiz,Art,Betrag\n1,1.1.,x,A,10\n2,2.1.,y,B,20\n,,,Summe,40",
            "",
        );
        assert_eq!(errors(&e).len(), 1, "{e:#?}");
    }

    #[test]
    fn review_2_only_a_real_taking_back_takes_back() {
        let names = |t: &str| required(t);
        assert_eq!(
            names("Spalten Datum und Betrag. Datum ist nicht optional."),
            ["Datum", "Betrag"]
        );
        assert_eq!(
            names("Spalten Datum und Steuer. Nicht Datum entfernen."),
            ["Datum", "Steuer"]
        );
        assert!(wanted("Ohne Steuer muss die Summe stimmen.").total);
        assert_eq!(
            names("Spalte Datum. Keine Summe, aber Spalte Betrag."),
            ["Datum", "Betrag"]
        );
        assert!(!wanted("Spalte Datum. Keine Summe, aber Spalte Betrag.").total);
        // Real ones still do.
        assert_eq!(names("Spalten Name, Datum. Entferne das Datum."), ["Name"]);
        assert_eq!(
            names("Spalten Name, Datum und Steuer. Datum entfernen, Steuer weg."),
            ["Name"]
        );
        assert_eq!(
            names("Spalten Firma, Betrag, nicht Datum."),
            ["Firma", "Betrag"]
        );
        assert!(!wanted("Without a total.").total);
        // Columns are whole words: an "Update" column is no "Date" column.
        let e = csv("Name,Update\nA,x", "columns Name and Date");
        assert_eq!(errors(&e).len(), 1, "{e:#?}");
        assert!(errors(&csv("Name,Date (UTC)\nA,x", "columns Name and Date")).is_empty());
    }

    #[test]
    fn review_1_a_word_file_that_breaks_off_is_not_readable() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file(
                "[Content_Types].xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            z.write_all(b"<Types/>").unwrap();
            z.start_file("_rels/.rels", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"<Relationships/>").unwrap();
            z.start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            z.write_all(b"<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>Test</w:t></w:r></w:p>")
                .unwrap();
            z.finish().unwrap();
        }
        assert!(layout("b.docx", buf.get_ref()).is_err());
        // Without its package description it is no Word file either.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            z.write_all(b"<w:document xmlns:w=\"w\"><w:body></w:body></w:document>")
                .unwrap();
            z.finish().unwrap();
        }
        assert!(layout("b.docx", buf.get_ref()).is_err());
    }

    #[test]
    fn review_2_a_word_package_is_whole_with_one_document() {
        use std::io::Write;
        let pack = |parts: &[(&str, &str)]| {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut z = zip::ZipWriter::new(&mut buf);
                for (name, body) in parts {
                    z.start_file(*name, zip::write::SimpleFileOptions::default())
                        .unwrap();
                    z.write_all(body.as_bytes()).unwrap();
                }
                z.finish().unwrap();
            }
            buf.into_inner()
        };
        let doc = "<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>Test</w:t></w:r></w:p></w:body></w:document>";
        let whole = [
            ("[Content_Types].xml", "<Types/>"),
            ("_rels/.rels", "<Relationships/>"),
            ("word/document.xml", doc),
        ];
        assert!(layout("a.docx", &pack(&whole)).is_ok());
        // Without where the document starts.
        assert!(layout("a.docx", &pack(&[whole[0], whole[2]])).is_err());
        // Two roots, or a root that is no document.
        let two = format!("{doc}{doc}");
        assert!(
            layout(
                "a.docx",
                &pack(&[whole[0], whole[1], ("word/document.xml", &two)])
            )
            .is_err()
        );
        let other = [
            whole[0],
            whole[1],
            ("word/document.xml", "<w:body xmlns:w=\"w\"></w:body>"),
        ];
        assert!(layout("a.docx", &pack(&other)).is_err());
        assert!(
            layout(
                "a.docx",
                &pack(&[whole[0], whole[1], ("word/document.xml", "")])
            )
            .is_err()
        );
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
