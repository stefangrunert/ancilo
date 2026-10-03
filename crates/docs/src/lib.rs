//! The user's documents (decision `2026-10-03-drei-bereiche`): files attached
//! to a chat are read – in a sandboxed process of their own, without network
//! and with a time limit – and kept as text; the chat takes the passages that
//! fit the question, each with its source (file, page or sheet).
//!
//! Only the text is kept, never a copy of the file. A conversation that saw
//! a document stays with the AI on this computer (see the assistant).

pub mod extract;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ancilo_core::{Error, Result};
use ancilo_storage::Db;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use extract::{Extracted, Kind, Locator, Part, Warning};

/// How long one document may take to read.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// What the extracting process may print (the text as JSON).
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
/// Text of attachments that goes to the model with one question.
pub const PASSAGE_BUDGET: usize = 12_000;

/// Reads documents: in a process of its own (the `ancilo` program,
/// `extract-document`) – sandboxed, without network, with a time limit – so
/// a broken or hostile file can neither hang nor crash Ancilo. Without that
/// program (tests) the reading happens here.
pub struct Extractor {
    bin: Option<PathBuf>,
    scratch: PathBuf,
    hidden: Vec<PathBuf>,
}

impl Extractor {
    /// `bin`: the `ancilo` program; `scratch`: where reading happens;
    /// `hidden`: places the reading process may not see (Ancilo's home).
    pub fn new(bin: Option<PathBuf>, scratch: PathBuf, hidden: Vec<PathBuf>) -> Self {
        Self {
            bin,
            scratch,
            hidden,
        }
    }

    /// A place of its own for one reading (the sandbox may write there).
    pub fn workdir(&self) -> Result<PathBuf> {
        let dir = self
            .scratch
            .join(format!("read-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(Error::internal)?;
        Ok(std::fs::canonicalize(&dir).unwrap_or(dir))
    }

    /// Reads `file`; `workdir` (from [`Self::workdir`]) is removed afterwards.
    pub async fn read(&self, file: &Path, workdir: &Path) -> Result<Extracted> {
        let result = match &self.bin {
            Some(bin) => self.read_apart(bin, file, workdir).await,
            None => {
                let file = file.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(|| extract::extract_file(&file))
                        .unwrap_or_else(|_| Err(Error::invalid("this file could not be read")))
                })
                .await
                .map_err(Error::internal)?
            }
        };
        std::fs::remove_dir_all(workdir).ok();
        result
    }

    async fn read_apart(&self, bin: &Path, file: &Path, workdir: &Path) -> Result<Extracted> {
        let script = format!(
            "exec {} extract-document {}",
            quote(&bin.display().to_string()),
            quote(&file.display().to_string())
        );
        let bounds = ancilo_agent::sandbox::Bounds {
            root: workdir,
            hidden: &self.hidden,
            network: false,
        };
        let mut cmd = match ancilo_agent::sandbox::command(&bounds, &script) {
            Ok(c) => c,
            // No sandbox on this system: still a process of its own.
            Err(_) => {
                let mut c = tokio::process::Command::new(bin);
                c.arg("extract-document").arg(file);
                c
            }
        };
        cmd.current_dir(workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(Error::internal)?;
        let out = match tokio::time::timeout(READ_TIMEOUT, child.wait_with_output()).await {
            Err(_) => {
                return Err(Error::invalid(
                    "reading this file took too long – it may be damaged",
                ));
            }
            Ok(r) => r.map_err(Error::internal)?,
        };
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("this file could not be read")
                .trim()
                .trim_start_matches("error: ")
                .to_string();
            return Err(Error::invalid(why));
        }
        if out.stdout.len() > MAX_OUTPUT {
            return Err(Error::invalid("this file holds too much text"));
        }
        serde_json::from_slice(&out.stdout).map_err(Error::internal)
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// An attached document as the app shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Pages with text (PDF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// Sheets (spreadsheets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheets: Option<u32>,
    pub chars: usize,
    #[serde(default)]
    pub warnings: Vec<Warning>,
    /// The conversation it belongs to, once it was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Attached documents, kept as text.
#[derive(Clone)]
pub struct Attachments {
    db: Db,
    extractor: Arc<Extractor>,
}

/// Only the name of a file, nothing of its path.
pub fn file_name(name: &str) -> String {
    let n = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let n: String = n.chars().filter(|c| !c.is_control()).take(200).collect();
    if n.is_empty() || n == "." || n == ".." {
        "document".into()
    } else {
        n
    }
}

impl Attachments {
    pub fn new(db: Db, extractor: Extractor) -> Self {
        Self {
            db,
            extractor: Arc::new(extractor),
        }
    }

    /// Reads a document the app sent (dragged in or chosen).
    pub async fn add_bytes(&self, name: &str, bytes: &[u8]) -> Result<AttachmentView> {
        let name = file_name(name);
        if extract::kind_of(&name).is_none() {
            // Says which kinds Ancilo reads.
            extract::extract(&name, b"")?;
        }
        if bytes.len() as u64 > extract::MAX_BYTES {
            return Err(Error::invalid(format!(
                "{name} is larger than {} MB – Ancilo does not read files this large",
                extract::MAX_BYTES / 1024 / 1024
            )));
        }
        // The file goes to the reading process's own place and is gone after.
        let dir = self.extractor.workdir()?;
        let file = dir.join(&name);
        std::fs::write(&file, bytes).map_err(Error::internal)?;
        let doc = self.extractor.read(&file, &dir).await?;
        self.save(&name, doc)
    }

    /// Reads a document from a path (the command line, tools).
    pub async fn add_path(&self, path: &Path) -> Result<AttachmentView> {
        if !path.is_absolute() || !path.is_file() {
            return Err(Error::invalid(format!(
                "{} is not a file – give its full path",
                path.display()
            )));
        }
        let name = file_name(&path.to_string_lossy());
        let dir = self.extractor.workdir()?;
        let doc = self.extractor.read(path, &dir).await?;
        self.save(&name, doc)
    }

    fn save(&self, name: &str, doc: Extracted) -> Result<AttachmentView> {
        let pages = doc
            .parts
            .iter()
            .filter_map(|p| match p.at {
                Some(Locator::Page(n)) => Some(n),
                _ => None,
            })
            .max();
        let sheets = doc
            .parts
            .iter()
            .filter(|p| matches!(p.at, Some(Locator::Sheet(_))))
            .count() as u32;
        let view = AttachmentView {
            id: format!("a-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: name.to_string(),
            kind: doc.kind,
            pages,
            sheets: (sheets > 0).then_some(sheets),
            chars: doc.chars(),
            warnings: doc.warnings.clone(),
            conversation: None,
            created_at: Utc::now(),
        };
        let meta = serde_json::to_string(&view)?;
        let parts = serde_json::to_string(&doc.parts)?;
        self.db.with(|c| {
            c.execute(
                "INSERT INTO attachments(id, created_at, conversation, name, meta, parts) VALUES(?1, ?2, NULL, ?3, ?4, ?5)",
                params![view.id, view.created_at.to_rfc3339(), view.name, meta, parts],
            )
            .map(|_| ())
        })?;
        Ok(view)
    }

    pub fn view(&self, id: &str) -> Result<AttachmentView> {
        let row: Option<(String, Option<String>)> = self.db.with(|c| {
            c.query_row(
                "SELECT meta, conversation FROM attachments WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })?;
        let (meta, conversation) =
            row.ok_or_else(|| Error::not_found(format!("no attachment '{id}'")))?;
        let mut v: AttachmentView = serde_json::from_str(&meta)?;
        v.conversation = conversation;
        Ok(v)
    }

    fn parts(&self, id: &str) -> Result<Vec<Part>> {
        let parts: Option<String> = self.db.with(|c| {
            c.query_row(
                "SELECT parts FROM attachments WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
        })?;
        Ok(serde_json::from_str(&parts.ok_or_else(|| {
            Error::not_found(format!("no attachment '{id}'"))
        })?)?)
    }

    /// The attachments go with a conversation; one already sent elsewhere
    /// cannot move (its text stays where it was shown).
    pub fn link(&self, ids: &[String], conversation: &str) -> Result<Vec<AttachmentView>> {
        let mut out = Vec::new();
        for id in ids {
            let v = self.view(id)?;
            if v.conversation.as_deref().is_some_and(|c| c != conversation) {
                return Err(Error::invalid(format!(
                    "{} belongs to another conversation – attach it again",
                    v.name
                )));
            }
            self.db.with(|c| {
                c.execute(
                    "UPDATE attachments SET conversation = ?2 WHERE id = ?1",
                    params![id, conversation],
                )
                .map(|_| ())
            })?;
            out.push(AttachmentView {
                conversation: Some(conversation.to_string()),
                ..v
            });
        }
        Ok(out)
    }

    /// Removes an attachment that was not sent yet (or any, by the user).
    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|c| {
            c.execute("DELETE FROM attachments WHERE id = ?1", params![id])
                .map(|_| ())
        })
    }

    /// A conversation is deleted: its documents' text goes with it.
    pub fn forget_conversation(&self, conversation: &str) -> Result<()> {
        self.db.with(|c| {
            c.execute(
                "DELETE FROM attachments WHERE conversation = ?1",
                params![conversation],
            )
            .map(|_| ())
        })
    }

    /// Attachments never sent are removed after a day.
    pub fn prune(&self) -> Result<usize> {
        let before = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        self.db.with(|c| {
            c.execute(
                "DELETE FROM attachments WHERE conversation IS NULL AND created_at < ?1",
                params![before],
            )
        })
    }

    /// The text for a question: everything when the documents are short,
    /// else the passages that fit `query` best – in document order, each with
    /// its source (`[file, page 3]`).
    pub fn passages(&self, ids: &[String], query: &str) -> Result<Vec<String>> {
        let mut chunks: Vec<(String, String)> = Vec::new();
        let mut whole: Vec<(String, String)> = Vec::new();
        for id in ids {
            let v = self.view(id)?;
            for p in self.parts(id)? {
                let label = source(&v.name, p.at.as_ref());
                for c in ancilo_web::rank::passages(&p.text) {
                    chunks.push((label.clone(), c));
                }
                whole.push((label, p.text));
            }
        }
        let total: usize = whole.iter().map(|(_, t)| t.chars().count()).sum();
        if total <= PASSAGE_BUDGET {
            return Ok(whole
                .into_iter()
                .filter(|(_, t)| !t.trim().is_empty())
                .map(|(l, t)| format!("{l}\n{}", t.trim()))
                .collect());
        }
        let texts: Vec<String> = chunks.iter().map(|(_, t)| t.clone()).collect();
        let scores = ancilo_web::rank::scores(&texts, query);
        let mut order: Vec<usize> = (0..chunks.len()).collect();
        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then(a.cmp(b)));
        let mut picked = Vec::new();
        let mut used = 0;
        for i in order {
            let n = chunks[i].1.chars().count();
            if used + n > PASSAGE_BUDGET {
                continue;
            }
            used += n;
            picked.push(i);
        }
        picked.sort();
        Ok(picked
            .into_iter()
            .map(|i| format!("{}\n{}", chunks[i].0, chunks[i].1))
            .collect())
    }
}

/// Where a passage comes from, as the model is asked to cite it.
fn source(name: &str, at: Option<&Locator>) -> String {
    match at {
        Some(Locator::Page(n)) => format!("[{name}, page {n}]"),
        Some(Locator::Sheet(s)) => format!("[{name}, sheet \"{s}\"]"),
        None => format!("[{name}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::tests::{docx_of, pdf_of};

    fn store() -> (tempfile::TempDir, Attachments) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::in_memory().unwrap();
        let ex = Extractor::new(None, dir.path().join("scratch"), Vec::new());
        (dir, Attachments::new(db, ex))
    }

    // covers: M10-AC-02
    #[tokio::test]
    async fn attachments_keep_their_text_and_give_passages_with_their_source() {
        let (_d, a) = store();
        let pdf = a
            .add_bytes(
                "../../Vertrag.pdf",
                &pdf_of(&["Die Miete betraegt 950 Euro.", "Kuendigung: drei Monate."]),
            )
            .await
            .unwrap();
        assert_eq!(
            (pdf.name.as_str(), pdf.kind, pdf.pages),
            ("Vertrag.pdf", Kind::Pdf, Some(2))
        );
        let doc = a
            .add_bytes("Brief.docx", &docx_of(&["Hallo Welt"]))
            .await
            .unwrap();
        // Short documents go along whole, each part with its source.
        let p = a
            .passages(&[pdf.id.clone(), doc.id.clone()], "Kuendigung")
            .unwrap();
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(
            p[1].starts_with("[Vertrag.pdf, page 2]\nKuendigung"),
            "{p:?}"
        );
        assert!(p[2].starts_with("[Brief.docx]\nHallo Welt"));
        // Long ones: the passages that fit the question.
        let filler: Vec<String> = (0..60)
            .map(|i| format!("Absatz {i} ueber das Wetter und den Garten. ").repeat(12))
            .collect();
        let mut pages: Vec<&str> = filler.iter().map(String::as_str).collect();
        pages.push("Die Kaution betraegt drei Monatsmieten.");
        let long = a.add_bytes("Lang.pdf", &pdf_of(&pages)).await.unwrap();
        let p = a
            .passages(std::slice::from_ref(&long.id), "Wie hoch ist die Kaution?")
            .unwrap();
        assert!(
            p.iter()
                .any(|x| x.starts_with("[Lang.pdf, page 61]") && x.contains("Kaution")),
            "{p:?}"
        );
        assert!(p.iter().map(|x| x.len()).sum::<usize>() <= PASSAGE_BUDGET + 40 * p.len());
        // Sent with a conversation, it stays there.
        a.link(std::slice::from_ref(&pdf.id), "c-1").unwrap();
        assert!(a.link(std::slice::from_ref(&pdf.id), "c-2").is_err());
        a.forget_conversation("c-1").unwrap();
        assert!(a.view(&pdf.id).is_err());
        // Nothing of the file stays behind.
        assert_eq!(
            std::fs::read_dir(_d.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn what_cannot_be_read_is_refused_with_the_reason() {
        let (_d, a) = store();
        let e = a.add_bytes("x.exe", b"MZ").await.unwrap_err();
        assert!(e.message().contains("reads PDF, Word"), "{}", e.message());
        let e = a.add_path(Path::new("relative.pdf")).await.unwrap_err();
        assert!(e.message().contains("full path"));
    }
}
