//! Text from the user's documents: PDF (page by page), Word, spreadsheets
//! (sheet by sheet) and plain text; pictures and scanned PDFs get their text
//! from recognition (see [`crate::ocr`], macOS). Runs in a process of its own (see
//! [`crate::Extractor`]): a broken file cannot take Ancilo down with it.
//!
//! Everything has limits – file size, unpacked size, text, rows – and what
//! was left out is said (`truncated`, `warnings`), never silently dropped.

use std::io::{Cursor, Read};
use std::path::Path;

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Files larger than this are not read.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Text kept of one document (characters).
pub const MAX_CHARS: usize = 1_000_000;
/// Rows kept of one sheet.
pub const MAX_ROWS: usize = 5_000;
/// What a part of a Word file may unpack to (a "zip bomb" stops here).
pub const MAX_UNPACKED: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Pdf,
    Word,
    Spreadsheet,
    /// A photo or scan (JPEG, PNG, HEIC, TIFF, WebP) – text by recognition.
    Image,
}

/// Where a part of a document is – for the source an answer names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Locator {
    Page(u32),
    Sheet(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Locator>,
    pub text: String,
}

/// Something worth saying about a document that was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    /// No text found – a scan or picture whose text could not be recognized.
    NoText,
    /// The text was recognized in a picture or scan – it may have mistakes.
    Recognized,
    /// Only the first part of the text was kept.
    Shortened,
    /// Only the first rows of a sheet were kept.
    RowsLeftOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Extracted {
    pub kind: Kind,
    pub parts: Vec<Part>,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

impl Extracted {
    pub fn chars(&self) -> usize {
        self.parts.iter().map(|p| p.text.chars().count()).sum()
    }
}

/// The kind of a file by its name; `None`: not a document Ancilo reads.
pub fn kind_of(name: &str) -> Option<Kind> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => Kind::Pdf,
        "docx" => Kind::Word,
        "xlsx" | "xlsm" | "xls" | "ods" => Kind::Spreadsheet,
        "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "log"
        | "rtf" | "html" | "htm" | "eml" => Kind::Text,
        // Text recognition is there on macOS only.
        "jpg" | "jpeg" | "png" | "heic" | "heif" | "tif" | "tiff" | "webp"
            if cfg!(target_os = "macos") =>
        {
            Kind::Image
        }
        _ => return None,
    })
}

/// Reads a document from a file.
pub fn extract_file(path: &Path) -> Result<Extracted> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let size = std::fs::metadata(path)
        .map_err(|e| Error::invalid(format!("cannot read {name}: {e}")))?
        .len();
    if size > MAX_BYTES {
        return Err(too_large(&name));
    }
    let bytes =
        std::fs::read(path).map_err(|e| Error::invalid(format!("cannot read {name}: {e}")))?;
    extract(&name, &bytes)
}

fn too_large(name: &str) -> Error {
    Error::invalid(format!(
        "{name} is larger than {} MB – Ancilo does not read files this large",
        MAX_BYTES / 1024 / 1024
    ))
}

/// Reads a document from its bytes; `name` decides how.
pub fn extract(name: &str, bytes: &[u8]) -> Result<Extracted> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large(name));
    }
    let kind = kind_of(name).ok_or_else(|| {
        Error::invalid(format!(
            "Ancilo cannot read {name} – it reads PDF, Word (.docx), Excel, CSV and text files{}",
            if cfg!(target_os = "macos") {
                ", and pictures (JPEG, PNG, HEIC)"
            } else {
                ""
            }
        ))
    })?;
    let mut doc = match kind {
        Kind::Pdf => pdf(name, bytes)?,
        Kind::Word => word(name, bytes)?,
        Kind::Spreadsheet => spreadsheet(name, bytes)?,
        // The text comes from recognition (in its own process, see `ocr`).
        Kind::Image => Extracted {
            kind,
            parts: Vec::new(),
            warnings: vec![Warning::NoText],
        },
        Kind::Text => Extracted {
            kind,
            parts: vec![Part {
                at: None,
                text: String::from_utf8_lossy(bytes).into_owned(),
            }],
            warnings: Vec::new(),
        },
    };
    limit(&mut doc);
    Ok(doc)
}

/// Keeps at most [`MAX_CHARS`] of text.
pub(crate) fn limit(doc: &mut Extracted) {
    let mut left = MAX_CHARS;
    let mut cut = false;
    doc.parts.retain_mut(|p| {
        if left == 0 {
            cut = true;
            return false;
        }
        let n = p.text.chars().count();
        if n > left {
            p.text = p.text.chars().take(left).collect();
            cut = true;
        }
        left = left.saturating_sub(n);
        true
    });
    if cut && !doc.warnings.contains(&Warning::Shortened) {
        doc.warnings.push(Warning::Shortened);
    }
}

fn pdf(name: &str, bytes: &[u8]) -> Result<Extracted> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes).map_err(|e| {
        Error::invalid(format!(
            "cannot read the PDF {name} ({e}) – it may be damaged or protected by a password"
        ))
    })?;
    let parts: Vec<Part> = pages
        .into_iter()
        .enumerate()
        .map(|(i, text)| Part {
            at: Some(Locator::Page(i as u32 + 1)),
            text: tidy(&text),
        })
        .collect();
    let mut warnings = Vec::new();
    if parts.iter().all(|p| p.text.trim().is_empty()) {
        warnings.push(Warning::NoText);
    }
    Ok(Extracted {
        kind: Kind::Pdf,
        parts: parts
            .into_iter()
            .filter(|p| !p.text.trim().is_empty())
            .collect(),
        warnings,
    })
}

/// Collapses the runs of blank lines PDF text often has.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

fn word(name: &str, bytes: &[u8]) -> Result<Extracted> {
    let bad = |e: String| Error::invalid(format!("cannot read the Word file {name} ({e})"));
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(e.to_string()))?;
    let entry = zip
        .by_name("word/document.xml")
        .map_err(|e| bad(e.to_string()))?;
    let mut xml = String::new();
    entry
        .take(MAX_UNPACKED)
        .read_to_string(&mut xml)
        .map_err(|e| bad(e.to_string()))?;
    let text = word_text(&xml).map_err(bad)?;
    Ok(Extracted {
        kind: Kind::Word,
        parts: vec![Part { at: None, text }],
        warnings: Vec::new(),
    })
}

/// The text of `word/document.xml`: paragraphs as lines, tabs and breaks kept.
fn word_text(xml: &str) -> std::result::Result<String, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) if e.local_name().into_inner() == "t" => in_text = true,
            Event::End(e) => match e.local_name().into_inner() {
                "t" => in_text = false,
                "p" => out.push('\n'),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().into_inner() {
                "tab" => out.push('\t'),
                "br" | "cr" => out.push('\n'),
                "p" => out.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => {
                out.push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) if in_text => {
                // &amp; and friends inside a text run
                out.push_str(match AsRef::<str>::as_ref(&r) {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(tidy(&out))
}

fn spreadsheet(name: &str, bytes: &[u8]) -> Result<Extracted> {
    use calamine::{Data, Reader, open_workbook_auto_from_rs};
    let mut book = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|e| Error::invalid(format!("cannot read the spreadsheet {name} ({e})")))?;
    let mut parts = Vec::new();
    let mut warnings = Vec::new();
    for sheet in book.sheet_names() {
        let Ok(range) = book.worksheet_range(&sheet) else {
            continue;
        };
        let mut text = String::new();
        for (i, row) in range.rows().enumerate() {
            if i >= MAX_ROWS {
                if !warnings.contains(&Warning::RowsLeftOut) {
                    warnings.push(Warning::RowsLeftOut);
                }
                break;
            }
            let cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    other => other.to_string(),
                })
                .collect();
            if cells.iter().all(String::is_empty) {
                continue;
            }
            text.push_str(cells.join("\t").trim_end());
            text.push('\n');
        }
        if !text.trim().is_empty() {
            parts.push(Part {
                at: Some(Locator::Sheet(sheet.clone())),
                text,
            });
        }
    }
    Ok(Extracted {
        kind: Kind::Spreadsheet,
        parts,
        warnings,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A PDF with one line of text per page.
    pub fn pdf_of(pages: &[&str]) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for text in pages {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*text)]),
                    Operation::new("ET", vec![]),
                ],
            };
            let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => stream,
                "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            });
            kids.push(page.into());
        }
        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        out.into_inner()
    }

    /// A Word file with these paragraphs.
    pub fn docx_of(paragraphs: &[&str]) -> Vec<u8> {
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<w:p><w:r><w:t xml:space=\"preserve\">{p}</w:t></w:r></w:p>"))
            .collect();
        zip_of(&[(
            "word/document.xml",
            &format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
            ),
        )])
    }

    /// An Excel file with one sheet of strings.
    pub fn xlsx_of(sheet: &str, rows: &[&[&str]]) -> Vec<u8> {
        let rows_xml: String = rows
            .iter()
            .enumerate()
            .map(|(r, cells)| {
                let cs: String = cells
                    .iter()
                    .enumerate()
                    .map(|(c, v)| {
                        format!(
                            "<c r=\"{}{}\" t=\"inlineStr\"><is><t>{v}</t></is></c>",
                            (b'A' + c as u8) as char,
                            r + 1
                        )
                    })
                    .collect();
                format!("<row r=\"{}\">{cs}</row>", r + 1)
            })
            .collect();
        zip_of(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                &format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="{sheet}" sheetId="1" r:id="rId1"/></sheets></workbook>"#
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                &format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows_xml}</sheetData></worksheet>"#
                ),
            ),
        ])
    }

    // covers: M10-AC-02
    #[test]
    fn reads_pdf_pages_word_paragraphs_sheets_and_text() {
        let d = extract(
            "Vertrag.pdf",
            &pdf_of(&["Miete 950 Euro", "Kuendigung drei Monate"]),
        )
        .unwrap();
        assert_eq!(d.kind, Kind::Pdf);
        assert_eq!(d.parts.len(), 2);
        assert_eq!(d.parts[1].at, Some(Locator::Page(2)));
        assert!(d.parts[1].text.contains("Kuendigung drei Monate"), "{d:?}");

        let d = extract(
            "Brief.docx",
            &docx_of(&["Sehr geehrte Damen &amp; Herren,", "danke."]),
        )
        .unwrap();
        assert_eq!(d.parts[0].text, "Sehr geehrte Damen & Herren,\ndanke.");

        let d = extract(
            "Umsatz.xlsx",
            &xlsx_of("Q1", &[&["Monat", "Umsatz"], &["Januar", "1200"]]),
        )
        .unwrap();
        assert_eq!(d.parts[0].at, Some(Locator::Sheet("Q1".into())));
        assert_eq!(d.parts[0].text, "Monat\tUmsatz\nJanuar\t1200\n");

        let d = extract("notes.md", "# Hallo\nWelt".as_bytes()).unwrap();
        assert_eq!(
            (d.kind, d.parts[0].text.as_str()),
            (Kind::Text, "# Hallo\nWelt")
        );
    }

    #[test]
    fn says_what_it_cannot_read_and_what_it_left_out() {
        let e = extract("song.mp3", b"x").unwrap_err();
        assert!(e.message().contains("reads PDF, Word"), "{}", e.message());
        let e = extract("broken.pdf", b"not a pdf").unwrap_err();
        assert!(
            e.message().contains("cannot read the PDF"),
            "{}",
            e.message()
        );
        let e = extract("broken.docx", b"not a zip").unwrap_err();
        assert!(
            e.message().contains("cannot read the Word file"),
            "{}",
            e.message()
        );
        // A scanned PDF: pages without text.
        let d = extract("scan.pdf", &pdf_of(&[""])).unwrap();
        assert_eq!(d.warnings, [Warning::NoText]);
        // Too much text: cut, and said so.
        let long = "a".repeat(MAX_CHARS + 10);
        let d = extract("long.txt", long.as_bytes()).unwrap();
        assert_eq!(d.chars(), MAX_CHARS);
        assert_eq!(d.warnings, [Warning::Shortened]);
    }
}
