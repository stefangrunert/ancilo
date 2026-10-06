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
        "tsv" => delimited(name, bytes, b'\t')?,
        "csv" => delimited(name, bytes, separator(&String::from_utf8_lossy(bytes)))?,
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

/// How a CSV separates its columns: the mark that splits its first
/// records into the same number of fields, two or more (a semicolon as
/// Excel writes it where the comma is the decimal mark; a "Name; Vorname"
/// inside a comma file does not); ties: the comma.
fn separator(text: &str) -> u8 {
    let head: String = text.lines().take(2000).collect::<Vec<_>>().join("\n");
    let score = |sep: u8| -> (bool, usize) {
        let Some(recs) = records(&head, sep as char) else {
            return (false, 0);
        };
        let recs: Vec<usize> = recs.iter().map(|(_, r)| r.len()).collect();
        // All alike (one column too): this mark.
        if recs.first().is_some_and(|&f| recs.iter().all(|&n| n == f)) {
            return (true, usize::MAX);
        }
        // A title line above the table does not count – but the fewer lines
        // a mark has to leave out, the likelier it is the one.
        let skipped = recs.iter().take_while(|&&n| n < 2).count();
        let body = &recs[skipped..];
        let Some(&first) = body.first() else {
            return (false, 0);
        };
        // Even: every record has the header's number of fields (a total
        // row too) – more fields prove nothing.
        (body.iter().all(|&n| n == first), usize::MAX - skipped)
    };
    b",;\t"
        .iter()
        .map(|&sep| (sep, score(sep)))
        .fold((b',', (false, 0)), |best, (sep, sc)| {
            if sc > best.1 { (sep, sc) } else { best }
        })
        .0
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
    let mut part = |path: &str| -> std::result::Result<String, String> {
        let entry = zip
            .by_name(path)
            .map_err(|_| "it is not a complete Word document".to_string())?;
        let mut xml = String::new();
        entry
            .take(crate::extract::MAX_UNPACKED)
            .read_to_string(&mut xml)
            .map_err(|e| e.to_string())?;
        if xml.len() as u64 >= crate::extract::MAX_UNPACKED {
            return Err("it is too large to be read whole".into());
        }
        Ok(xml)
    };
    // A Word package as Word opens it: its parts named with their types,
    // where its document starts, and that document a Word document.
    let types = part("[Content_Types].xml").map_err(bad)?;
    let rels = part("_rels/.rels").map_err(bad)?;
    let main = package_start(&types, &rels).map_err(bad)?;
    let xml = part(&main).map_err(bad)?;
    Ok(Layout {
        kind: Kind::Word,
        sheets: Vec::new(),
        blocks: word_blocks(&xml).map_err(bad)?,
        limits: Vec::new(),
    })
}

/// The document part a Word package starts with – from its relationships,
/// of a Word document's type.
fn package_start(types: &str, rels: &str) -> std::result::Result<String, String> {
    const NOT_WORD: &str = "it is not a Word document";
    let mut defaults = Vec::new();
    let mut overrides = Vec::new();
    xml_elements(
        types,
        (
            "Types",
            "http://schemas.openxmlformats.org/package/2006/content-types",
        ),
        |name, attrs| {
            let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
            match name {
                "Default" => defaults.push((
                    get("Extension").unwrap_or_default().to_lowercase(),
                    get("ContentType").unwrap_or_default(),
                )),
                "Override" => overrides.push((
                    get("PartName").unwrap_or_default(),
                    get("ContentType").unwrap_or_default(),
                )),
                _ => {}
            }
        },
    )?;
    let mut start = None;
    xml_elements(
        rels,
        (
            "Relationships",
            "http://schemas.openxmlformats.org/package/2006/relationships",
        ),
        |name, attrs| {
            let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
            if name == "Relationship"
                && get("Type").is_some_and(|t| t.ends_with("/officeDocument"))
                && get("TargetMode").is_none_or(|m| m != "External")
            {
                start = get("Target");
            }
        },
    )?;
    let target = start.ok_or(NOT_WORD)?;
    let path = target.trim_start_matches('/').to_string();
    if path.is_empty() || path.split('/').any(|p| p == ".." || p == ".") {
        return Err(NOT_WORD.into());
    }
    let ext = path.rsplit('.').next().unwrap_or_default().to_lowercase();
    let kind = overrides
        .iter()
        .find(|(p, _)| p.trim_start_matches('/').eq_ignore_ascii_case(&path))
        .or_else(|| defaults.iter().find(|(e, _)| *e == ext))
        .map(|(_, t)| t.as_str())
        .unwrap_or_default();
    let word = [
        "wordprocessingml.document.main+xml",
        "wordprocessingml.template.main+xml",
        "ms-word.document.macroenabled.main+xml",
        "ms-word.template.macroenabledtemplate.main+xml",
    ];
    if !word.iter().any(|w| kind.to_lowercase().ends_with(w)) {
        return Err(NOT_WORD.into());
    }
    Ok(path)
}

/// Runs `each` over the elements of `xml` (local name, attributes by local
/// name) – checking it is well-formed XML with the root `root`.
fn xml_elements(
    xml: &str,
    root: (&str, &str),
    mut each: impl FnMut(&str, &[(String, String)]),
) -> std::result::Result<(), String> {
    let (root, namespace) = root;
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut open, mut roots) = (0i64, 0);
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        let (e, empty) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(_) => {
                open -= 1;
                continue;
            }
            Event::Eof => break,
            Event::Text(t) if open == 0 && !t.xml10_content().trim().is_empty() => {
                return Err("it is not a Word document".into());
            }
            Event::CData(_) if open == 0 => return Err("it is not a Word document".into()),
            _ => continue,
        };
        let name = e.local_name().into_inner().to_string();
        // Every attribute well-formed (none twice, values that read); the
        // root's namespace as its prefix (or none) declares it.
        let prefix = e.name().prefix().map(|p| p.into_inner().to_string());
        let declares = match &prefix {
            Some(p) => format!("xmlns:{p}"),
            None => "xmlns".to_string(),
        };
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut xmlns = None;
        for a in e.attributes() {
            let a = a.map_err(|_| "it is not a Word document".to_string())?;
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|_| "it is not a Word document".to_string())?
                .to_string();
            if a.key.into_inner() == declares {
                xmlns = Some(v.clone());
            }
            attrs.push((a.key.local_name().into_inner().to_string(), v));
        }
        if open == 0 {
            roots += 1;
            if roots > 1 || name != root || xmlns.as_deref() != Some(namespace) {
                return Err("it is not a Word document".into());
            }
        }
        each(&name, &attrs);
        if !empty {
            open += 1;
        }
    }
    if open != 0 || roots != 1 {
        return Err("it is not a Word document".into());
    }
    Ok(())
}

/// Whether an element is in Word's namespace (as its prefix is declared on
/// it).
fn word_namespace(e: &quick_xml::events::BytesStart) -> bool {
    const WORD: [&str; 2] = [
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        "http://purl.oclc.org/ooxml/wordprocessingml/main",
    ];
    let name = e.name();
    let prefix = name.prefix().map(|p| p.into_inner().to_string());
    let want = match &prefix {
        Some(p) => format!("xmlns:{p}"),
        None => "xmlns".into(),
    };
    e.attributes().all(|a| a.is_ok())
        && e.attributes().flatten().any(|a| {
            a.key.into_inner() == want
                && a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .is_ok_and(|v| WORD.contains(&v.as_ref()))
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
            // Nothing but space outside the document.
            Event::Text(t) if open == 0 && !t.xml10_content().trim().is_empty() => {
                return Err("it is not a Word document".into());
            }
            Event::CData(_) if open == 0 => return Err("it is not a Word document".into()),
            // Every element's attributes well-formed (none twice).
            Event::Start(e) | Event::Empty(e) if e.attributes().any(|a| a.is_err()) => {
                return Err("it is not a Word document".into());
            }
            Event::Start(e) | Event::Empty(e) if open == 0 => {
                roots += 1;
                if roots > 1 || e.local_name().into_inner() != "document" || !word_namespace(e) {
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
    // A description's items are columns when the file says so: most of them
    // are its column heads ("deliveries.csv with Order, Carrier and Delivery
    // date" – Order and Carrier are). Then a missing one is missing.
    let fields: Vec<&Term> = w.terms.iter().filter(|t| t.need == Need::Field).collect();
    let as_column = |t: &Term| {
        present(
            l,
            &Term {
                text: t.text.clone(),
                need: Need::Column,
            },
        )
    };
    let columns = fields.iter().filter(|t| as_column(t)).count();
    let fields_are_columns = columns >= 1 && columns * 2 >= fields.len();
    let (maybe, sure): (Vec<&Term>, Vec<&Term>) = missing
        .iter()
        .partition(|t| t.need == Need::Field && !fields_are_columns);
    if !sure.is_empty() {
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Error,
            msg("check.missing", &[("what", &list(&sure))]),
            None,
        ));
    }
    if !maybe.is_empty() {
        out.push(find(
            CheckArea::Complete,
            CheckLevel::Warning,
            msg("check.maybe_missing", &[("what", &list(&maybe))]),
            None,
        ));
    }
    if missing.is_empty() && !w.terms.is_empty() {
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
    // What the task names, filled in every row that has the rest.
    for t in &tables {
        let Some(h) = header_row(t) else {
            continue;
        };
        for term in w
            .terms
            .iter()
            .filter(|t| t.need == Need::Column && !may_be_empty(task, &t.text))
        {
            let Some(c) = t.rows[h]
                .cells
                .iter()
                .position(|x| has_words(x, &term.text))
            else {
                continue;
            };
            let empty = t.rows[h + 1..].iter().filter(|r| {
                !is_total_row(r)
                    && r.cells.get(c).is_none_or(|x| x.trim().is_empty())
                    && r.values.get(c).is_none_or(Option::is_none)
                    && r.cells.iter().filter(|x| !x.trim().is_empty()).count() >= 2
            });
            // A hint: an empty cell may be meant (a group's rows below its
            // first one).
            for r in empty.take(3) {
                out.push(find(
                    CheckArea::Complete,
                    CheckLevel::Warning,
                    msg("check.missing_value", &[("what", &t.rows[h].cells[c])]),
                    Some(t.at(r.number, c)),
                ));
            }
        }
    }
    // A total row without a number ("wird ergänzt") is no total; one with a
    // formula is (Ancilo keeps formulas as text – said below, not changed).
    // Any word for a total counts here ("Gesamtkosten", "Overall budget",
    // "Equipment total", "Beide Monate"): missing is only what no row and
    // no sentence of the file names in any way.
    let has_total = tables.iter().any(|t| {
        t.rows.iter().any(|r| {
            (is_total_row(r) || label_of(r).is_some_and(|l| ANY_TOTAL.is_match(l)))
                && (0..r.cells.len())
                    .any(|c| value(r, c).is_some() || r.cells[c].trim_start().starts_with('='))
        })
    });
    let stated = stated_totals(l);
    if w.total && !has_total && stated.is_empty() {
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
    // A total the text states ("Die Gesamtsumme beträgt 108,00 EUR")
    // against the table's amounts.
    // What it refers to is not sure (another table, all of them, a count of
    // people): a hint, never an error.
    let mut amounts: Vec<Vec<Num>> = tables.iter().filter_map(table_total).collect();
    if amounts.len() > 1 {
        amounts.push(amounts.concat());
    }
    for said in stated.iter().filter(|_| !amounts.is_empty()) {
        let fits = amounts.iter().any(|a| {
            [Way::De, Way::En].iter().any(|&way| {
                let sum: f64 = a.iter().map(|n| n.get(way)).sum();
                said.iter().any(|n| close(sum, n.get(way)))
            })
        });
        if !fits {
            let sum: f64 = amounts[0].iter().map(|n| n.get(Way::De)).sum();
            out.push(find(
                CheckArea::Numbers,
                CheckLevel::Warning,
                msg(
                    "check.text_total",
                    &[("shown", &show(said[0].get(Way::De))), ("sum", &show(sum))],
                ),
                None,
            ));
        }
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
    /// Named in a description of a table ("eine Liste mit A, B und C") –
    /// fields or contents, which is not sure: a column or anywhere in it,
    /// and when not there a hint, never an error.
    Field,
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
        r"(?i)\b(summe|summenzeile|gesamt|gesamtsumme|gesamtbetrag|teilsumme|teilsummen|zwischensumme|zwischensummen|insgesamt|total|totals|subtotal|subtotals|sum)\b",
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
/// Words that take back what follows them in their clause ("Do not add
/// totals", "No aggregate total is required").
const CLAUSE_NO: &[&str] = &[
    "no", "not", "nicht", "kein", "keine", "keinen", "keiner", "never", "nie", "don't", "dont",
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
    next.first().is_some_and(|w| NO_AFTER.contains(&w.as_str())) && !turned_around(&prev)
}

/// Whether a "not" stands right before a name (articles and "Spalte"
/// between are fine): "nicht die Spalte Datum entfernen".
fn turned_around(before: &[String]) -> bool {
    before
        .iter()
        .rev()
        .find(|w| !FILLER.contains(&w.as_str()))
        .is_some_and(|w| NOT.contains(&w.as_str()))
}

/// Whether `name` stands in `text` as whole words.
fn has_words(text: &str, name: &str) -> bool {
    let (t, n) = (words_of(text), words_of(name));
    !n.is_empty() && t.windows(n.len()).any(|w| w == n.as_slice())
}

static LIST: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(spalten|spaltenüberschriften|columns|column headers|felder|fields|überschriften|abschnitten|abschnitte|sections|headings)\b\s*(?:[:\-–]|für|for|namens|named|wie|like)?\s*([^.;!?\n]+)",
    )
    .expect("valid")
});
/// One column or section, named after it.
static SINGLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(spalte|column|abschnitt|section|heading|überschrift)\b\s*(?:[:\-–]|namens|named)?\s*")
        .expect("valid")
});
/// Where a named item ends.
static ITEM_END: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\s+(und|and|sowie|mit|with|als|as|für|for|in|im|on)\s|[,:(]")
        .expect("valid")
});

/// A table described by what it holds: "Liste mit A, B und C", "eine
/// Tabelle für A, B und C".
static FIELDS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(tabelle|liste|\w+liste|übersicht|csv|excel(?:-\w+)?|table|list|spreadsheet|sheet)\b[^.;!?\n]{0,40}?\b(?:mit|with|für|for)\s+([^.;!?\n:]+)",
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
        // Lists of what to hold: after "Spalten", "Abschnitte" … – or, in a
        // table's description, after "mit"/"für" when it is a list of three
        // or more ("eine Liste mit Rechnungsnummer, Datum und Betrag").
        let mut lists: Vec<(Need, usize, usize, &str)> = LIST
            .captures_iter(sentence)
            .map(|m| {
                let trigger = m[1].to_lowercase();
                let need = if [
                    "überschriften",
                    "abschnitten",
                    "abschnitte",
                    "abschnitt",
                    "sections",
                    "section",
                    "headings",
                    "heading",
                ]
                .contains(&trigger.as_str())
                {
                    Need::Section
                } else {
                    Need::Column
                };
                let (t, i) = (m.get(1).unwrap(), m.get(2).unwrap());
                (need, t.start(), i.start(), i.as_str())
            })
            .collect();
        // "einen Abschnitt Befund und einen Abschnitt Nächste Schritte",
        // "heading Estimate and a table …": a singular names one thing –
        // up to the next "und", "and", comma.
        for m in SINGLE.captures_iter(sentence) {
            let (t, all) = (m.get(1).unwrap(), m.get(0).unwrap());
            let rest = &sentence[all.end()..];
            let cut = ITEM_END.find(rest).map_or(rest.len(), |e| e.start());
            let trigger = t.as_str().to_lowercase();
            let need = if ["spalte", "column"].contains(&trigger.as_str()) {
                Need::Column
            } else {
                Need::Section
            };
            lists.push((need, t.start(), all.end(), &rest[..cut]));
        }
        if lists.is_empty()
            && let Some(m) = FIELDS.captures(sentence)
            && m[2].contains(',')
            && m[2]
                .split([','])
                .flat_map(|p| p.split(" und ").flat_map(|q| q.split(" and ")))
                .count()
                >= 3
        {
            let (t, i) = (m.get(1).unwrap(), m.get(2).unwrap());
            lists.push((Need::Field, t.start(), i.start(), i.as_str()));
        }
        let ranges: Vec<(usize, usize)> = lists
            .iter()
            .map(|(_, _, at, items)| (*at, at + items.len()))
            .collect();
        // Inside a list as a name of its own (`Gesamt`, `Line total EUR`) –
        // not as part of a phrase ("und unbedingt eine Gesamtsumme am Ende").
        let in_list = |i: usize| {
            ranges.iter().any(|&(a, b)| {
                if !(a..b).contains(&i) {
                    return false;
                }
                let from = sentence[a..i]
                    .rfind([',', '/', '&'])
                    .map_or(a, |k| a + k + 1);
                let to = sentence[i..b]
                    .find([',', '/', '&', ':'])
                    .map_or(b, |k| i + k);
                let item = sentence[from..to]
                    .split(" und ")
                    .flat_map(|q| q.split(" and "))
                    .find(|q| {
                        let start = q.as_ptr() as usize - sentence.as_ptr() as usize;
                        (start..start + q.len()).contains(&i)
                    })
                    .unwrap_or("");
                let words = item
                    .split_whitespace()
                    .filter(|w| !STOP.contains(&w.to_lowercase().as_str()))
                    .count();
                words <= 3
            })
        };
        for (need, trigger_at, items_at, items) in lists {
            // "ohne Spalten …": the whole list is taken back.
            let list_negated = no_before(&sentence[..trigger_at]);
            // "Nicht die Spalte Datum entfernen": what follows is kept.
            let list_turned = turned_around(&words_of(&sentence[..items_at]));
            for item in items.split([',', '/', '&']).flat_map(|p| {
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
                // "Datum weg" – but not "Datum nicht entfernen".
                let item_after = all.len() >= 2
                    && NO_AFTER.contains(&lower(&all[all.len() - 1]).as_str())
                    && !NOT.contains(&lower(&all[all.len() - 2]).as_str())
                    && !list_turned;
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
                // A column name is a few words; more – or a verb – is a
                // sentence going on ("Belegnotizen wäre praktisch").
                if !(1..=3).contains(&words.len())
                    || words[0].starts_with(['„', '"'])
                    || words
                        .iter()
                        .any(|w| VERB.contains(&w.to_lowercase().as_str()))
                {
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
        // "Keine Summe, sondern Gesamtsumme": one mention that asks for a
        // total is enough; the sentence takes it back only when every one
        // says no.
        // A total's word inside a list of columns, or as a column's name
        // ("Line total EUR"), asks for no total row; a "no", "not", "kein"
        // anywhere before it in its clause ("Do not add totals", "No
        // aggregate total is required") takes it back.
        let said: Vec<bool> = TOTAL_WORD
            .find_iter(sentence)
            .filter(|m| !in_list(m.start()))
            // The name of a column asked for ("Hinterlege Gesamt als Formel").
            .filter(|m| {
                !w.terms
                    .iter()
                    .any(|t| t.need == Need::Column && t.text.eq_ignore_ascii_case(m.as_str()))
            })
            .filter(|m| {
                let before = words_of(&sentence[..m.start()]);
                let after = words_of(&sentence[m.end()..]);
                !(before.last().is_some_and(|w| w == "line" || w == "zeilen")
                    || after
                        .first()
                        .is_some_and(|w| ["eur", "usd", "gbp", "chf"].contains(&w.as_str())))
            })
            .map(|m| {
                let clause_start = sentence[..m.start()]
                    .rfind([',', ';', ':'])
                    .map_or(0, |i| i + 1);
                let clause = words_of(&sentence[clause_start..m.start()]);
                let clause_no = clause.iter().any(|w| CLAUSE_NO.contains(&w.as_str()));
                // "… ist nicht verlangt", "… is not required".
                let clause_end = sentence[m.end()..]
                    .find([',', ';', ':'])
                    .map_or(sentence.len(), |k| m.end() + k);
                let rest = words_of(&sentence[m.end()..clause_end]);
                let not_wanted = rest.windows(2).any(|p| {
                    ["nicht", "not", "kein"].contains(&p[0].as_str())
                        && [
                            "verlangt",
                            "nötig",
                            "notwendig",
                            "erforderlich",
                            "gewünscht",
                            "required",
                            "needed",
                            "necessary",
                            "wanted",
                        ]
                        .contains(&p[1].as_str())
                });
                let clause_no = clause_no || not_wanted;
                !(clause_no
                    || no_before(&sentence[..m.start()])
                    || no_after(&sentence[..m.start()], &sentence[m.end()..]))
            })
            .collect();
        if !said.is_empty() {
            w.total = said.contains(&true);
        }
    }
    w
}

/// The names a task asks for (any place) – see [`wanted`].
pub fn required(task: &str) -> Vec<String> {
    wanted(task).terms.into_iter().map(|t| t.text).collect()
}

/// Words that make a phrase a sentence, not a name.
const VERB: &[&str] = &[
    "ist", "sind", "wäre", "wären", "muss", "müssen", "soll", "sollen", "kann", "können", "darf",
    "sein", "werden", "wird", "hätte", "habe", "is", "are", "be", "would", "should", "must", "may",
    "can", "will", "need", "needs",
];

const STOP: &[&str] = &[
    "den",
    "der",
    "die",
    "das",
    "dem",
    "des",
    "ein",
    "eine",
    "einen",
    "einer",
    "jeweils",
    "je",
    "the",
    "a",
    "abschnitt",
    "section",
    "spalte",
    "column",
    "an",
    "each",
    "for",
    "für",
    "mit",
    "with",
    "in",
    "im",
    "zu",
    "to",
];

/// Whether `t` is where it belongs in `l`.
fn present(l: &Layout, t: &Term) -> bool {
    let want = t.text.to_lowercase();
    // Whole words: an "Update" column is no "Date" column.
    let has = |s: &str| has_words(s, &t.text);
    match t.need {
        Need::Quoted => has(&all_text(l)),
        Need::Field => {
            present(
                l,
                &Term {
                    text: t.text.clone(),
                    need: Need::Column,
                },
            ) || has(&all_text(l))
        }
        Need::Column => {
            // A header: a sheet's first row with two cells or more (a title
            // above it does not count), a table's first row.
            l.sheets.iter().any(|s| {
                let t = Table {
                    name: None,
                    rows: s.rows.clone(),
                };
                header_row(&t).is_some_and(|h| t.rows[h].cells.iter().any(|c| has(c)))
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

/// Which way a cell's text writes a number, where only one way reads it
/// (1.234,50 · 250,50 German; 1,234.50 · 250.50 English); `None`: either,
/// or no marks.
fn style(cell: &str) -> Option<Way> {
    if !matches!(read_number(cell)?, Num::Sure(_)) {
        return None;
    }
    let s: String = cell.chars().filter(|c| matches!(c, '.' | ',')).collect();
    let (dots, commas) = (s.matches('.').count(), s.matches(',').count());
    match (dots, commas) {
        (0, 0) => None,
        (0, 1) => Some(Way::De),
        (1, 0) => Some(Way::En),
        (_, 0) => Some(Way::De),
        (0, _) => Some(Way::En),
        _ if s.ends_with(',') => Some(Way::De),
        _ => Some(Way::En),
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
    // One column, one way of writing numbers: 1.234,50 next to 250.50 is
    // read by no program as meant.
    for c in 0..cols {
        let mut seen: Vec<(Way, u32, &str)> = Vec::new();
        for r in t.rows.iter().skip(first) {
            if r.values.get(c).is_some_and(Option::is_some) {
                continue;
            }
            if let Some(cell) = r.cells.get(c)
                && let Some(w) = style(cell)
                && !seen.iter().any(|(x, ..)| *x == w)
            {
                seen.push((w, r.number, cell.as_str()));
            }
        }
        // A hint: Ancilo reads both, another program may not.
        if let [(_, _, a), (_, row, b)] = seen.as_slice() {
            out.push(find(
                CheckArea::Numbers,
                CheckLevel::Warning,
                msg(
                    "check.mixed_formats",
                    &[("what", &name_of(header, c)), ("a", a), ("b", b)],
                ),
                Some(t.at(*row, c)),
            ));
        }
    }
    // A total: a subtotal against the rows of its group; a grand total
    // against all rows (or the totals before and what came after them); a
    // plain "Summe"/"Total" against any of these.
    let mut seg_from = first;
    let mut totals: Vec<Vec<Num>> = vec![Vec::new(); cols];
    let mut after_sub: Vec<usize> = vec![first; cols];
    let mut sums: Vec<Sum> = Vec::new();
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
            let parts: Vec<Vec<Num>> = match kind {
                Total::Sub => vec![seg],
                Total::Grand => vec![all, subs_then],
                // Without any rows above: nothing of its own to add up.
                Total::Any if all.is_empty() => Vec::new(),
                Total::Any => vec![seg, all, subs_then],
            }
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect();
            if kind != Total::Grand {
                col_totals.push(total);
                after_sub[c] = ri + 1;
            }
            if parts.is_empty() {
                // Nothing above it to add up (a total right after a total):
                // said, not passed over.
                out.push(find(
                    CheckArea::Numbers,
                    CheckLevel::Warning,
                    msg("check.total_alone", &[("what", &name_of(header, c))]),
                    Some(t.at(r.number, c)),
                ));
                continue;
            }
            sums.push(Sum {
                row: r.number,
                col: c,
                total,
                parts,
            });
        }
        seg_from = ri + 1;
    }
    // Quantity × price = amount, row by row.
    let col = |re: &regex::Regex| header.iter().position(|h| re.is_match(h.trim()));
    let mut products: Vec<Product> = Vec::new();
    if let (Some(q), Some(p), Some(a)) = (col(&QTY), col(&PRICE), col(&AMOUNT))
        && q != a
        && p != a
    {
        for r in t.rows.iter().skip(first).filter(|r| !is_total_row(r)) {
            if let (Some(qv), Some(pv), Some(av)) = (value(r, q), value(r, p), value(r, a)) {
                products.push(Product {
                    row: r.number,
                    cols: [q, p, a],
                    nums: [qv, pv, av],
                });
            }
        }
    }
    // One way of reading each column, for all its totals and amounts
    // together (1,250 is either 1.25 or 1250 – the same in every row of a
    // column; another column may write its numbers otherwise): the reading
    // under which the fewest are wrong.
    let ways = readings(cols, &sums, &products);
    checked += sums.len() + products.len();
    for s in &sums {
        let way = ways[s.col];
        if s.right(way) {
            continue;
        }
        wrong += 1;
        let sum: f64 = s.parts[0].iter().map(|n| n.get(way)).sum();
        out.push(find(
            CheckArea::Numbers,
            CheckLevel::Error,
            msg(
                "check.total_wrong",
                &[
                    ("what", &name_of(header, s.col)),
                    ("shown", &show(s.total.get(way))),
                    ("sum", &show(sum)),
                ],
            ),
            Some(t.at(s.row, s.col)),
        ));
    }
    for p in &products {
        if p.right(&ways) {
            continue;
        }
        wrong += 1;
        let [q, pr, a] = p.cols;
        let (qd, pd, ad) = (
            p.nums[0].get(ways[q]),
            p.nums[1].get(ways[pr]),
            p.nums[2].get(ways[a]),
        );
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
            Some(t.at(p.row, a)),
        ));
    }
    (checked, wrong)
}

/// Whether the task allows `name` to stay empty ("Datum ist optional",
/// "may be left empty").
fn may_be_empty(task: &str, name: &str) -> bool {
    static OPTIONAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(optional|leer bleiben|leer lassen|darf leer|kann leer|nicht ausgefüllt|nicht immer|may be (left )?empty|can be (left )?empty|need not|not required|if known|falls bekannt|wenn vorhanden)\b")
            .expect("valid")
    });
    task.split(['.', ';', '!', '?', '\n'])
        .any(|s| has_words(s, name) && OPTIONAL.is_match(s))
}

/// The header of a table: the first of its first three rows with two cells
/// or more (a title above it does not count).
fn header_row(t: &Table) -> Option<usize> {
    let filled = |r: &Row| r.cells.iter().filter(|c| !c.trim().is_empty()).count();
    // A table of one column: its first row.
    if t.rows.iter().all(|r| filled(r) <= 1) {
        return t.rows.iter().position(|r| filled(r) == 1);
    }
    t.rows.iter().take(3).position(|r| filled(r) >= 2)
}

/// What a table's amounts come to: its total row's amount, or the amounts
/// of its rows – for a table with an amount column.
fn table_total(t: &Table) -> Option<Vec<Num>> {
    let h = header_row(t)?;
    let a = t.rows[h]
        .cells
        .iter()
        .position(|c| AMOUNT.is_match(c.trim()))?;
    let rows = &t.rows[h + 1..];
    if let Some(total) = rows
        .iter()
        .rev()
        .find(|r| matches!(total_kind(r), Some(Total::Grand | Total::Any)))
        .and_then(|r| value(r, a))
    {
        return Some(vec![total]);
    }
    let all: Vec<Num> = rows
        .iter()
        .filter(|r| !is_total_row(r))
        .filter_map(|r| value(r, a))
        .collect();
    (!all.is_empty()).then_some(all)
}

/// The totals a document's text states: for each, the numbers of its
/// sentence after the word (one of them is the total – "die Summe der 3
/// Posten beträgt 96").
fn stated_totals(l: &Layout) -> Vec<Vec<Num>> {
    l.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { text } => Some(text),
            _ => None,
        })
        .flat_map(|text| {
            STATED_TOTAL.find_iter(text).filter_map(|m| {
                let rest = &text[m.end()..];
                let sentence = rest.split(". ").next().unwrap_or(rest);
                let nums: Vec<Num> = NUMBER
                    .find_iter(sentence)
                    .filter_map(|n| read_number(n.as_str()))
                    .collect();
                (!nums.is_empty()).then_some(nums)
            })
        })
        .collect()
}

static STATED_TOTAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(gesamtsumme|gesamtbetrag|endsumme|summe|insgesamt|grand total|total)\b",
    )
    .expect("valid")
});
static NUMBER: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"-?\d[\d.,']*\d|\d").expect("valid"));

fn name_of(header: &[String], c: usize) -> String {
    header
        .get(c)
        .filter(|h| !h.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| column_letter(c))
}

/// A total and what it may add up to (any one of `parts`).
struct Sum {
    row: u32,
    col: usize,
    total: Num,
    parts: Vec<Vec<Num>>,
}

impl Sum {
    fn right(&self, way: Way) -> bool {
        self.parts
            .iter()
            .any(|p| close(p.iter().map(|n| n.get(way)).sum(), self.total.get(way)))
    }
}

/// Quantity × price = amount in one row.
struct Product {
    row: u32,
    /// Quantity, price, amount.
    cols: [usize; 3],
    nums: [Num; 3],
}

impl Product {
    fn right(&self, ways: &[Way]) -> bool {
        let g = |i: usize| self.nums[i].get(ways[self.cols[i]]);
        close(g(0) * g(1), g(2))
    }
}

/// The reading of each column under which the fewest totals and amounts
/// are wrong – every combination for the columns that can be read two ways
/// (up to eight of them; more: one column after the other while it helps).
/// Ties: German first.
fn readings(cols: usize, sums: &[Sum], products: &[Product]) -> Vec<Way> {
    let two_ways = |c: usize| {
        sums.iter().filter(|s| s.col == c).any(|s| {
            matches!(s.total, Num::Either { .. })
                || s.parts
                    .iter()
                    .flatten()
                    .any(|n| matches!(n, Num::Either { .. }))
        }) || products.iter().any(|p| {
            p.cols
                .iter()
                .zip(p.nums)
                .any(|(&pc, n)| pc == c && matches!(n, Num::Either { .. }))
        })
    };
    let open: Vec<usize> = (0..cols).filter(|&c| two_ways(c)).collect();
    let wrong = |ways: &[Way]| {
        sums.iter().filter(|s| !s.right(ways[s.col])).count()
            + products.iter().filter(|p| !p.right(ways)).count()
    };
    let mut best = vec![Way::De; cols];
    if open.len() <= 8 {
        let mut least = usize::MAX;
        for bits in 0u32..(1 << open.len()) {
            let mut ways = vec![Way::De; cols];
            for (i, &c) in open.iter().enumerate() {
                if bits & (1 << i) != 0 {
                    ways[c] = Way::En;
                }
            }
            let w = wrong(&ways);
            if w < least {
                least = w;
                best = ways;
            }
        }
        return best;
    }
    let mut least = wrong(&best);
    loop {
        let mut better = false;
        for &c in &open {
            let mut ways = best.clone();
            ways[c] = Way::En;
            if ways[c] != best[c] {
                let w = wrong(&ways);
                if w < least {
                    least = w;
                    best = ways;
                    better = true;
                }
            }
        }
        if !better {
            return best;
        }
    }
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
    let label = r
        .cells
        .iter()
        .map(String::as_str)
        .find(|c| total_label(c))?;
    let l = label.trim().to_lowercase();
    Some(
        if ["teilsumme", "zwischensumme", "subtotal", "sub-total"]
            .iter()
            .any(|w| l.starts_with(w) || l.ends_with(w))
        {
            Total::Sub
        } else if [
            "gesamt",
            "endsumme",
            "grand total",
            "overall total",
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
    r.cells.iter().any(|c| total_label(c))
}

/// What names a row: its first cell with text (not a number) – a total's
/// word in a later note ("A, Gesamtsumme folgt unten, 10") names nothing.
fn label_of(r: &Row) -> Option<&str> {
    r.cells
        .iter()
        .enumerate()
        .find(|(c, x)| {
            !x.trim().is_empty()
                && r.values.get(*c).is_none_or(Option::is_none)
                && read_number(x).is_none()
        })
        .map(|(_, x)| x.as_str())
}

/// A cell that names a total: the word alone (with a "netto", a currency),
/// or a word that can mean nothing else with what it is of ("Zwischensumme
/// Nord", "Subtotal Q1") – never "Total service".
fn total_label(cell: &str) -> bool {
    let c = cell.trim();
    TOTAL_LABEL.is_match(c)
        || (c.chars().count() <= 40 && (QUALIFIED_TOTAL.is_match(c) || TOTAL_AFTER.is_match(c)))
}

/// A total named by what it is of, before the word ("Travel subtotal",
/// "Equipment total", "Monatssumme").
static TOTAL_AFTER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(\S+\s+){1,2}(subtotal|sub-total|total|zwischensumme|teilsumme|summe)$|^\S+(summe|gesamt)$")
        .expect("valid")
});

/// A label that may name a total, however worded – for whether there is
/// one, never for what it adds up.
static ANY_TOTAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(summe|gesamt|insgesamt|total|overall|grand|sum\b|beide\b|alle\b|both\b|all\b)",
    )
    .expect("valid")
});

static QUALIFIED_TOTAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        // What it is of names a thing: capitalized or a number ("Zwischensumme
        // Nord", "Subtotal Q1") – "Gesamtsumme folgt unten" is a sentence.
        r"^((?i:zwischensumme|teilsumme|gesamtsumme|endsumme|subtotal|sub-total)(\s+[\p{Lu}\d]\S*){1,2}|(?i:grand total)\s+\d{4})$",
    )
    .expect("valid")
});

static TOTAL_LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(summe|gesamt|gesamtsumme|gesamtbetrag|insgesamt|zwischensumme|teilsumme|endsumme|total|subtotal|sub-total|grand total|sum)(\s+(netto|brutto|eur|€|usd|\$|\(.*\)))?\s*:?$",
    )
    .expect("valid")
});

static QTY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(menge|anzahl|stück|stk\.?|stunden|std\.?|qty|quantity|units|hours|hrs)\b",
    )
    .expect("valid")
});
static PRICE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(einzelpreis|stückpreis|stundensatz|preis|satz|unit price|unit cost|price|rate|ep)\b").expect("valid")
});
static AMOUNT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(betrag|gesamt|gesamtpreis|summe|total|amount|line total|line amount|line value|value|wert|gp)\b")
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
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Menge</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Preis</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Betrag</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>5</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>11</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
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
    fn review_3_one_reading_per_column_for_totals_and_amounts_together() {
        let n = |text: &str| errors(&csv(text, "")).len();
        // 1,250 as 1.25 in one row and as 1250 in the next: no.
        assert_eq!(
            n("Quantity,Price,Amount\n2,\"1,250\",2.5\n2,\"1,250\",2500\nGrand total,,2502.5"),
            1
        );
        // The amount as 1.25 for the product, as 1250 for the total: no.
        assert_eq!(
            n("Quantity,Price,Amount\n2,0.625,\"1,250\"\nGrand total,,1250"),
            1
        );
        // Each column its own way: fine.
        assert_eq!(n("Quantity,Price,Amount\n2,\"1,250\",2.500"), 0);
        assert_eq!(
            n("Item,Amount\nA,\"1,250\"\nB,\"2,500\"\nGrand total,\"3,750\""),
            0
        );
    }

    #[test]
    fn review_3_every_total_is_checked_or_said_to_be_not() {
        // "Gesamt" is of everything.
        assert_eq!(
            errors(&csv("Item,Amount\nA,10\nSubtotal,10\nB,20\nGesamt,20", "")).len(),
            1
        );
        // A grand total over plain totals.
        assert_eq!(
            errors(&csv(
                "Item,Amount\nA,10\nTotal,10\nB,20\nTotal,20\nGrand total,999",
                ""
            ))
            .len(),
            1
        );
        assert_eq!(
            errors(&csv("Item,Amount\nTotal,10\nTotal,20\nGrand total,999", "")).len(),
            1
        );
        // A total with no rows of its own: said.
        let f = csv(
            "Item,Amount\nA,10\nSubtotal,10\nSubtotal,999\nGrand total,10",
            "",
        );
        assert!(
            f.iter()
                .any(|x| x.level == CheckLevel::Warning && x.place.as_deref() == Some("t.csv!B4")),
            "{f:#?}"
        );
    }

    #[test]
    fn round_3_tables_as_people_write_them() {
        let n = |text: &str, task: &str| errors(&csv(text, task)).len();
        let hints = |text: &str, task: &str| {
            csv(text, task)
                .into_iter()
                .filter(|f| f.level == CheckLevel::Warning)
                .collect::<Vec<_>>()
        };
        // Semicolons, as Excel writes CSV where the comma is the decimal mark.
        assert_eq!(
            n("Posten;Betrag\nA;86,50\nB;13,50\nGesamtsumme;105,00", ""),
            1
        );
        assert_eq!(
            n(
                "Posten;Betrag\nA;86,50\nB;13,50\nGesamtsumme;100,00",
                "mit einer Gesamtsumme"
            ),
            0
        );
        // … but a semicolon inside a field of a comma file is no separator
        // (review 4, 38).
        assert_eq!(
            n(
                "Name; Vorname; Titel,Amount\nA,10\nB,20\nTotal,30",
                "Spalten Amount und einer Summe"
            ),
            0
        );
        // A subtotal named with what it is of.
        assert_eq!(
            n(
                "Region,Status,Betrag\nN,ok,125.5\nN,ok,74.5\n,Zwischensumme Nord,200\nS,ok,90\nS,ok,110\n,Zwischensumme Süd,200\n,Gesamtsumme,400",
                ""
            ),
            0
        );
        // "Total service" stays an item, and so does a note that starts with
        // a total's word (review 4, 43).
        assert_eq!(n("Item,Amount\nTotal service,50\nB,20\nTotal,70", ""), 0);
        assert_eq!(
            n(
                "Artikel,Hinweis,Betrag\nA,Gesamtsumme folgt unten,10\nB,,20\nGesamtsumme,,30",
                ""
            ),
            0
        );
        assert_eq!(
            n("Item,Amount\nGrand total service,50\nB,20\nTotal,70", ""),
            0
        );
        // Totals named by what they are of, the word last.
        assert_eq!(
            n(
                "Category,Item,GBP\nTravel,Train,75\nTravel,Bus,25\nTravel subtotal,,90\nMeals,Lunch,18\nMeals,Dinner,22\nMeals subtotal,,40\nOverall total,,140",
                ""
            ),
            1
        );
        // One column, two ways of writing numbers: a hint – both are read
        // right (review 4, 42).
        let mixed = "Projekt;Betrag\nA;1.234,50\nB;250.50\nGesamtsumme;1.485,00";
        assert_eq!(n(mixed, ""), 0);
        assert_eq!(hints(mixed, "").len(), 1);
        // What a description of a table lists: not found, a hint – it may be
        // contents, not columns (review 4, 39).
        let task =
            "Erstelle die Rechnungsliste mit Rechnungsnummer, Rechnungsdatum und Betrag (EUR).";
        assert_eq!(
            n(
                "Rechnungsnummer,Lieferdatum,Betrag\nR-1,2026-09-30,48",
                task
            ),
            0
        );
        assert_eq!(
            hints(
                "Rechnungsnummer,Lieferdatum,Betrag\nR-1,2026-09-30,48",
                task
            )
            .len(),
            1
        );
        let names = "Name,Betrag\nAlice,10\nBob,20\nCharlie,30";
        assert!(
            csv(names, "Erstelle eine Liste mit Alice, Bob und Charlie")
                .iter()
                .all(|f| f.level == CheckLevel::Ok)
        );
        assert_eq!(
            n(
                "Firma,Betrag\nA,10",
                "Erstelle eine Tabelle mit den Rechnungen."
            ),
            0
        );
        // A named column empty in a row that has the rest: a hint – unless
        // the task allows it (review 4, 41).
        let rows = "Beleg,Datum,Betrag\nK-31,2026-10-03,18\nK-32,,22";
        assert_eq!(n(rows, "Spalten Beleg, Datum und Betrag"), 0);
        let h = hints(rows, "Spalten Beleg, Datum und Betrag");
        assert_eq!(h.len(), 1, "{h:#?}");
        assert_eq!(h[0].place.as_deref(), Some("t.csv!B3"));
        assert!(
            hints(
                rows,
                "Spalten Beleg, Datum und Betrag. Datum ist optional und darf leer bleiben."
            )
            .is_empty()
        );
    }

    #[test]
    fn review_5_what_is_sure_and_what_is_not() {
        let n = |text: &str, task: &str| errors(&csv(text, task)).len();
        // A comma file whose first field holds semicolons, with its total (49).
        let mut f = String::from("Name; Vorname; Titel,Amount\n");
        for i in 1..=19 {
            f.push_str(&format!("A{i:02};X;Y,10\n"));
        }
        f.push_str("Total,190\n");
        assert_eq!(n(&f, "mit einer Gesamtsumme"), 0);
        // Subtotals beside their category (50).
        assert_eq!(
            n(
                "Category,Item,Amount\nTravel,Train,75\nTravel,Bus,25\nTravel,Subtotal,100\nMeals,Lunch,18\nMeals,Dinner,22\nMeals,Subtotal,40\n,Grand total,140",
                ""
            ),
            0
        );
        assert_eq!(
            n(
                "Artikel,Betrag\nGesamtsumme folgt unten,10\nB,20\nGesamtsumme,30",
                ""
            ),
            0
        );
        // One column (54).
        assert_eq!(n("Name\nAlpha;Beta\nGamma;Delta", "Spalte Name"), 0);
        // No total asked for (round 5 cases).
        for task in [
            "Create contacts.csv with Customer ID, Organisation and Contact. Do not add totals.",
            "Create stock.xlsx with columns SKU, Opening, Closing. No aggregate total is required.",
            "On Lines, use columns Item, Quantity, Unit price EUR, Line total EUR.",
        ] {
            assert!(!wanted(task).total, "{task}");
        }
        assert!(wanted("Ergänze eine Zeile Gesamt mit der Besucherzahl.").total);
        assert!(wanted("Keine Summe, sondern Gesamtsumme.").total);
        assert!(!wanted("Spalten Artikel, Anzahl, Gesamt. Hinterlege Gesamt als Formel. Eine Gesamtsumme ist nicht verlangt.").total);
        assert!(
            wanted(
                "Eine Excel-Datei mit Leistung und Betrag, und unbedingt eine Gesamtsumme am Ende."
            )
            .total
        );
        // One section each, after a singular.
        assert_eq!(
            required("Ich brauche einen Abschnitt Befund und einen Abschnitt Nächste Schritte."),
            ["Befund", "Nächste Schritte"]
        );
        assert_eq!(
            required("Create estimate.docx with heading Estimate and a table Service, Hours."),
            ["Estimate"]
        );
        // Hours × rate.
        assert_eq!(
            n(
                "Service,Hours,Rate EUR,Amount EUR\nTranslation,4,60,260",
                ""
            ),
            1
        );
        // Items most of which are columns: the rest are missing columns.
        assert_eq!(
            n(
                "Order,Carrier\nSO-61,Parcel Post",
                "Create deliveries.csv with Order, Carrier and Delivery date."
            ),
            1
        );
        assert_eq!(
            n(
                "Name,Betrag\nAlice,10\nBob,20\nCharlie,30",
                "Erstelle eine Liste mit Alice, Bob und Charlie"
            ),
            0
        );
    }

    #[test]
    fn round_3_a_total_the_text_states_matches_the_table() {
        let doc = |tables: &[&[(&str, &str)]], para: &str| {
            let mut blocks: Vec<Block> = tables
                .iter()
                .map(|rows| Block::Table {
                    rows: std::iter::once(vec!["Posten".to_string(), "Betrag".to_string()])
                        .chain(rows.iter().map(|(a, b)| vec![a.to_string(), b.to_string()]))
                        .collect(),
                })
                .collect();
            blocks.push(Block::Paragraph { text: para.into() });
            let l = Layout {
                kind: Kind::Word,
                sheets: Vec::new(),
                blocks,
                limits: Vec::new(),
            };
            check(&l, "mit der Gesamtsumme im Text")
        };
        let level = |f: &[Finding]| {
            f.iter()
                .filter(|x| x.level != CheckLevel::Ok)
                .map(|x| x.level)
                .collect::<Vec<_>>()
        };
        let one: &[(&str, &str)] = &[("Essen", "96,00")];
        // Not matching: a hint (what the text refers to is not sure).
        assert_eq!(
            level(&doc(&[one], "Die Gesamtsumme beträgt 108,00 EUR.")),
            [CheckLevel::Warning]
        );
        assert!(level(&doc(&[one], "Die Gesamtsumme beträgt 96,00 EUR.")).is_empty());
        assert!(level(&doc(&[one], "Die Summe der 12 Essen beträgt 96 Euro.")).is_empty());
        // Both tables together; subtotals are not the total (review 4, 40).
        let t: &[(&str, &str)] = &[("A", "10"), ("B", "20")];
        assert!(
            level(&doc(
                &[t, t],
                "Die Gesamtsumme beider Tabellen beträgt 60 EUR."
            ))
            .is_empty()
        );
        let subs: &[(&str, &str)] = &[
            ("A", "10"),
            ("Zwischensumme Nord", "10"),
            ("B", "20"),
            ("Zwischensumme Süd", "20"),
        ];
        assert!(level(&doc(&[subs], "Die Gesamtsumme beträgt 30 EUR.")).is_empty());
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
        // Review 3: a "not" before the name, or before the verb.
        assert_eq!(
            names("Spalten Datum und Betrag. Spalte Datum nicht entfernen."),
            ["Datum", "Betrag"]
        );
        assert_eq!(
            names("Spalten Datum und Betrag. Nicht die Spalte Datum entfernen."),
            ["Datum", "Betrag"]
        );
        assert!(wanted("Keine Summe, sondern Gesamtsumme.").total);
        assert!(!wanted("Keine Summe und keine Teilsummen.").total);
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
        const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let types = |part: &str| {
            format!(
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/{part}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#
            )
        };
        let rels = |target: &str| {
            format!(
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="{target}"/></Relationships>"#
            )
        };
        let doc = |ns: &str| {
            format!(
                r#"<w:document xmlns:w="{ns}"><w:body><w:p><w:r><w:t>Test</w:t></w:r></w:p></w:body></w:document>"#
            )
        };
        let (t, r, d) = (
            types("word/document.xml"),
            rels("word/document.xml"),
            doc(W),
        );
        let ok = |parts: &[(&str, &str)]| layout("a.docx", &pack(parts)).is_ok();
        assert!(ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &d)
        ]));
        // Another place, as its relationship says.
        let (t2, r2) = (types("word/main.xml"), rels("/word/main.xml"));
        assert!(ok(&[
            ("[Content_Types].xml", &t2),
            ("_rels/.rels", &r2),
            ("word/main.xml", &d)
        ]));
        // Without where the document starts.
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("word/document.xml", &d)
        ]));
        // Package parts that are no XML, or not what they should be (review 3).
        assert!(!ok(&[
            ("[Content_Types].xml", "garbage"),
            ("_rels/.rels", "garbage"),
            ("word/document.xml", &d)
        ]));
        assert!(!ok(&[
            ("[Content_Types].xml", "<Types/>"),
            ("_rels/.rels", "<Relationships/>"),
            ("word/document.xml", &d)
        ]));
        // Starting at a part that is not there – another one beside it does not count.
        let (t3, r3) = (types("missing.xml"), rels("missing.xml"));
        assert!(!ok(&[
            ("[Content_Types].xml", &t3),
            ("_rels/.rels", &r3),
            ("word/document.xml", &d)
        ]));
        // The document of the wrong kind or in another namespace.
        let plain = t.replace("officedocument.wordprocessingml.document.main+xml", "plain");
        assert!(!ok(&[
            ("[Content_Types].xml", &plain),
            ("_rels/.rels", &r),
            ("word/document.xml", &d)
        ]));
        let alien = doc("urn:alien");
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &alien)
        ]));
        // Two roots, a root that is no document, nothing.
        let two = format!("{d}{d}");
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &two)
        ]));
        let body = format!(r#"<w:body xmlns:w="{W}"></w:body>"#);
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &body)
        ]));
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", "")
        ]));
        // Review 4: an attribute twice, text after the document, package
        // parts in another namespace.
        let twice = r.replace(
            r#"Target="word/document.xml""#,
            r#"Target="word/document.xml" Target="missing.xml""#,
        );
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &twice),
            ("word/document.xml", &d)
        ]));
        let after = format!("{d}garbage");
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &after)
        ]));
        let alien_types = t.replace(
            "http://schemas.openxmlformats.org/package/2006/content-types",
            "urn:alien",
        );
        assert!(!ok(&[
            ("[Content_Types].xml", &alien_types),
            ("_rels/.rels", &r),
            ("word/document.xml", &d)
        ]));
        // Review 5: prefixed package namespaces are the same; an attribute
        // twice inside, CDATA after the document are not.
        let ct = t
            .replace("<Types xmlns=", "<ct:Types xmlns:ct=")
            .replace("</Types>", "</ct:Types>")
            .replace("<Default ", "<ct:Default ")
            .replace("<Override ", "<ct:Override ");
        assert!(ok(&[
            ("[Content_Types].xml", &ct),
            ("_rels/.rels", &r),
            ("word/document.xml", &d)
        ]));
        let pr = r
            .replace("<Relationships xmlns=", "<pr:Relationships xmlns:pr=")
            .replace("</Relationships>", "</pr:Relationships>")
            .replace("<Relationship ", "<pr:Relationship ");
        assert!(ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &pr),
            ("word/document.xml", &d)
        ]));
        let inner = d.replacen("<w:p>", r#"<w:p test="a" test="b">"#, 1);
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &inner)
        ]));
        let cdata = format!("{d}<![CDATA[garbage]]>");
        assert!(!ok(&[
            ("[Content_Types].xml", &t),
            ("_rels/.rels", &r),
            ("word/document.xml", &cdata)
        ]));
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
