//! FPL-03: does checking a result find what is wrong – and accept what is
//! right? The cases of `evals/fpl03/` (a file, the task, what must be
//! found) through `preview::layout` and `preview::check`, as Ancilo looks at
//! a task's result.
//!
//! `FPL03_CASES=<file>` runs another set, `FPL03_REPORT=<file>` writes JSON.

use std::io::Write;
use std::path::PathBuf;

use ancilo_docs::preview::{CheckLevel, Finding, check, layout};
use serde_json::{Value, json};

/// The file a case describes.
fn file(f: &Value) -> (String, Vec<u8>) {
    let name = f["name"].as_str().unwrap().to_string();
    let ext = name.rsplit('.').next().unwrap().to_ascii_lowercase();
    let bytes = match ext.as_str() {
        "xlsx" => xlsx(f["sheets"].as_array().unwrap()),
        "docx" => docx(f["blocks"].as_array().unwrap()),
        _ => f["text"].as_str().unwrap_or_default().as_bytes().to_vec(),
    };
    (name, bytes)
}

fn cell_at(start: &str) -> (u32, u16) {
    let letters: String = start
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let row: u32 = start[letters.len()..].parse().unwrap_or(1);
    let col = letters
        .to_ascii_uppercase()
        .bytes()
        .fold(0u32, |acc, b| acc * 26 + u32::from(b - b'A' + 1));
    (row - 1, (col.max(1) - 1) as u16)
}

fn xlsx(sheets: &[Value]) -> Vec<u8> {
    let mut book = rust_xlsxwriter::Workbook::new();
    for s in sheets {
        let ws = book.add_worksheet();
        if let Some(n) = s["name"].as_str() {
            ws.set_name(n).unwrap();
        }
        let (r0, c0) = cell_at(s["start"].as_str().unwrap_or("A1"));
        for (r, row) in s["rows"].as_array().unwrap().iter().enumerate() {
            for (c, v) in row.as_array().unwrap().iter().enumerate() {
                let (r, c) = (r0 + r as u32, c0 + c as u16);
                match v {
                    Value::Number(n) => {
                        ws.write_number(r, c, n.as_f64().unwrap()).unwrap();
                    }
                    Value::String(t) if !t.is_empty() => {
                        ws.write_string(r, c, t).unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
    book.save_to_buffer().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn para(style: Option<String>, text: &str) -> String {
    let ppr = style.map_or(String::new(), |s| {
        format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>")
    });
    format!(
        "<w:p>{ppr}<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        esc(text)
    )
}

fn docx(blocks: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for b in blocks {
        if let Some(h) = b["heading"].as_str() {
            body.push_str(&para(
                Some(format!("Heading{}", b["level"].as_u64().unwrap_or(1))),
                h,
            ));
        } else if let Some(p) = b["paragraph"].as_str() {
            body.push_str(&para(None, p));
        } else if let Some(rows) = b["table"].as_array() {
            body.push_str("<w:tbl>");
            for r in rows {
                body.push_str("<w:tr>");
                for c in r.as_array().unwrap() {
                    let t = match c {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    body.push_str(&format!("<w:tc>{}</w:tc>", para(None, &t)));
                }
                body.push_str("</w:tr>");
            }
            body.push_str("</w:tbl>");
        }
    }
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut out);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("[Content_Types].xml", o).unwrap();
        z.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        z.start_file("_rels/.rels", o).unwrap();
        z.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(doc.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    out.into_inner()
}

/// What a check got wrong against a case (empty: right).
fn misses(expect: &Value, findings: &[Finding]) -> Vec<String> {
    let errors: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.level == CheckLevel::Error)
        .collect();
    let mut m = Vec::new();
    if expect["valid"] == true {
        m.extend(
            errors
                .iter()
                .map(|e| format!("wrongly: {} {:?}", e.message, e.place)),
        );
        return m;
    }
    for want in expect["errors"].as_array().into_iter().flatten() {
        let area = want["area"].as_str().unwrap_or_default();
        let place = want["place"].as_str();
        let found = errors.iter().any(|e| {
            serde_json::to_value(e.area).unwrap() == area
                && place.is_none_or(|p| e.place.as_deref() == Some(p))
        });
        if !found {
            m.push(format!("not found: {area} {}", place.unwrap_or("")));
        }
    }
    m
}

// covers: FPL-03 (checking results)
#[test]
fn checks_find_what_is_wrong_and_accept_what_is_right() {
    let path = std::env::var("FPL03_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/fpl03/dev-cases.json")
        });
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (mut ok, mut rows) = (0, Vec::new());
    let (mut valid_ok, mut valid_all, mut wrong_ok, mut wrong_all) = (0, 0, 0, 0);
    println!("FPL-03 · {} · {} cases", path.display(), cases.len());
    for c in &cases {
        let (name, bytes) = file(&c["file"]);
        let task = c["task"].as_str().unwrap_or_default();
        let findings = match layout(&name, &bytes) {
            Ok(l) => check(&l, task),
            Err(e) => ancilo_docs::preview::unreadable(&e.message()),
        };
        let m = misses(&c["expect"], &findings);
        let valid = c["expect"]["valid"] == true;
        if valid {
            valid_all += 1;
        } else {
            wrong_all += 1;
        }
        if m.is_empty() {
            ok += 1;
            if valid {
                valid_ok += 1;
            } else {
                wrong_ok += 1;
            }
        }
        println!(
            "{:<12} {:<18} {:<6} {}",
            c["id"].as_str().unwrap(),
            c["group"].as_str().unwrap_or_default(),
            if m.is_empty() { "ok" } else { "✗" },
            m.join("; ")
        );
        rows.push(json!({"id": c["id"], "passed": m.is_empty(), "misses": m, "findings": findings.iter().map(|f| json!({"area": f.area, "level": f.level, "message": f.message, "place": f.place})).collect::<Vec<_>>()}));
    }
    println!(
        "passed {ok}/{} · valid accepted {valid_ok}/{valid_all} · wrong found {wrong_ok}/{wrong_all}",
        cases.len()
    );
    if let Ok(out) = std::env::var("FPL03_REPORT") {
        std::fs::write(out, serde_json::to_string_pretty(&json!({"cases": path.display().to_string(), "passed": ok, "valid_accepted": [valid_ok, valid_all], "wrong_found": [wrong_ok, wrong_all], "rows": rows})).unwrap()).unwrap();
    }
    if std::env::var("FPL03_CASES").is_err() {
        assert_eq!(ok, cases.len(), "a development case fails");
    }
}
