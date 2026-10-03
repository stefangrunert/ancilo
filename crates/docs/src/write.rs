//! New documents an agent writes for the user (decision
//! `2026-10-03-drei-bereiche`): spreadsheets (`.xlsx`) and letters or
//! reports (`.docx`). Numbers stay numbers; nothing is ever a formula –
//! a cell that looks like one is written as text.

use std::io::{Cursor, Write};

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Sheet {
    pub name: String,
    /// Rows of cells; the first row is usually the header.
    pub rows: Vec<Vec<String>>,
}

/// A spreadsheet: one sheet per entry, the first row bold.
pub fn xlsx(sheets: &[Sheet]) -> Result<Vec<u8>> {
    use rust_xlsxwriter::{Format, Workbook};
    if sheets.is_empty() {
        return Err(Error::invalid("give at least one sheet"));
    }
    let mut book = Workbook::new();
    let bold = Format::new().set_bold();
    for s in sheets {
        let ws = book.add_worksheet();
        let name: String = s
            .name
            .chars()
            .filter(|c| !"[]:*?/\\".contains(*c))
            .take(31)
            .collect();
        if !name.trim().is_empty() {
            ws.set_name(name.trim())
                .map_err(|e| Error::invalid(e.to_string()))?;
        }
        for (r, row) in s.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let (r, c) = (r as u32, c as u16);
                let number = cell
                    .trim()
                    .replace(',', ".")
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite());
                let res = match number {
                    // "1.234,50" or "12 €" stay text: only plain numbers become numbers.
                    Some(n) if !cell.trim().starts_with('+') => {
                        if r == 0 {
                            ws.write_number_with_format(r, c, n, &bold).map(|_| ())
                        } else {
                            ws.write_number(r, c, n).map(|_| ())
                        }
                    }
                    // Text – never a formula, even if it starts with "=".
                    _ if r == 0 => ws.write_string_with_format(r, c, cell, &bold).map(|_| ()),
                    _ => ws.write_string(r, c, cell).map(|_| ()),
                };
                res.map_err(|e| Error::invalid(e.to_string()))?;
            }
        }
        ws.autofit();
    }
    book.save_to_buffer()
        .map_err(|e| Error::internal(e.to_string()))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A Word document: an optional title, then paragraphs (`# ` starts a
/// heading, empty lines separate paragraphs).
pub fn docx(title: Option<&str>, text: &str) -> Result<Vec<u8>> {
    let mut body = String::new();
    let para = |style: Option<&str>, t: &str| {
        let ppr = style.map_or(String::new(), |s| {
            format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>")
        });
        let runs: Vec<String> = t
            .split('\n')
            .map(|line| format!("<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(line)))
            .collect();
        format!("<w:p>{ppr}{}</w:p>", runs.join("<w:r><w:br/></w:r>"))
    };
    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        body.push_str(&para(Some("Title"), t.trim()));
    }
    for block in text.split("\n\n") {
        let block = block.trim_end();
        if block.trim().is_empty() {
            continue;
        }
        if let Some(h) = block.strip_prefix("# ") {
            body.push_str(&para(Some("Heading1"), h.trim()));
        } else if let Some(h) = block.strip_prefix("## ") {
            body.push_str(&para(Some("Heading2"), h.trim()));
        } else {
            body.push_str(&para(None, block));
        }
    }
    let files: [(&str, String); 4] = [
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#.into()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.into()),
        ("word/styles.xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:after="160"/></w:pPr><w:rPr><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="40"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="30"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="26"/></w:rPr></w:style></w:styles>"#.into()),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#)),
    ];
    let mut out = Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, content) in &files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .map_err(|e| Error::internal(e.to_string()))?;
            z.write_all(content.as_bytes()).map_err(Error::internal)?;
        }
        z.finish().map_err(|e| Error::internal(e.to_string()))?;
    }
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Locator, extract};

    // covers: M10-AC-05
    #[test]
    fn written_documents_read_back_as_written_and_formulas_stay_text() {
        let x = xlsx(&[Sheet {
            name: "Rechnungen".into(),
            rows: vec![
                vec!["Firma".into(), "Betrag".into()],
                vec!["Stadtwerke".into(), "120.5".into()],
                vec!["=HYPERLINK(\"http://evil\")".into(), "12 €".into()],
            ],
        }])
        .unwrap();
        let d = extract("t.xlsx", &x).unwrap();
        assert_eq!(d.parts[0].at, Some(Locator::Sheet("Rechnungen".into())));
        assert_eq!(
            d.parts[0].text,
            "Firma\tBetrag\nStadtwerke\t120.5\n=HYPERLINK(\"http://evil\")\t12 €\n"
        );
        let w = docx(
            Some("Kündigung"),
            "# Betreff\n\nSehr geehrte Damen & Herren,\nhiermit kündige ich.",
        )
        .unwrap();
        let d = extract("k.docx", &w).unwrap();
        assert_eq!(
            d.parts[0].text,
            "Kündigung\nBetreff\nSehr geehrte Damen & Herren,\nhiermit kündige ich."
        );
    }
}
