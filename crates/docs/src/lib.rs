//! The user's documents (decision `2026-10-03-drei-bereiche`): files attached
//! to a chat are read – in a sandboxed process of their own, without network
//! and with a time limit – and kept as text; the chat takes the passages that
//! fit the question, each with its source (file, page or sheet).
//!
//! Only the text is kept, never a copy of the file. A conversation that saw
//! a document stays with the AI on this computer (see the assistant).

pub mod evidence;
pub mod extract;
pub mod library;
pub mod ocr;
pub mod preview;
pub mod split;
pub mod write;

use std::collections::HashSet;
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
    /// The text recognition helper (`None`: pictures and scans stay without text).
    ocr: Option<PathBuf>,
}

impl Extractor {
    /// `bin`: the `ancilo` program; `scratch`: where reading happens;
    /// `hidden`: places the reading process may not see (Ancilo's home).
    pub fn new(bin: Option<PathBuf>, scratch: PathBuf, hidden: Vec<PathBuf>) -> Self {
        Self {
            bin,
            scratch,
            hidden,
            ocr: None,
        }
    }

    /// Recognizes text in pictures and scanned PDFs with `helper` (see [`ocr`]).
    pub fn with_ocr(mut self, helper: Option<PathBuf>) -> Self {
        self.ocr = helper;
        self
    }

    /// Whether pictures and scans get their text recognized here.
    pub fn recognizes(&self) -> bool {
        self.ocr.is_some()
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
        let result = self.read_in(file, workdir).await;
        std::fs::remove_dir_all(workdir).ok();
        result
    }

    async fn read_in(&self, file: &Path, workdir: &Path) -> Result<Extracted> {
        let mut doc = match &self.bin {
            Some(bin) => {
                let file = self.inside(file, workdir)?;
                self.read_apart(bin, &file, workdir).await?
            }
            None => {
                let file = file.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(|| extract::extract_file(&file))
                        .unwrap_or_else(|_| Err(Error::invalid("this file could not be read")))
                })
                .await
                .map_err(Error::internal)??
            }
        };
        // A picture or a scan: its text by recognition.
        if let Some(helper) = &self.ocr
            && ocr::wanted(&doc)
        {
            let file = self.inside(file, workdir)?;
            match ocr::recognize(helper, &file, workdir).await {
                Ok(pages) => ocr::merge(&mut doc, pages),
                Err(e) => tracing::warn!(error = %e.message(), "text recognition failed"),
            }
        }
        Ok(doc)
    }

    /// `file` inside `workdir` – the reading processes see only their own
    /// place: a document from elsewhere is put there first (a clone on APFS).
    /// Ancilo's own data is never handed over.
    fn inside(&self, file: &Path, workdir: &Path) -> Result<PathBuf> {
        if file.starts_with(workdir) {
            return Ok(file.to_path_buf());
        }
        let real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        if self
            .hidden
            .iter()
            .any(|h| real.starts_with(std::fs::canonicalize(h).unwrap_or_else(|_| h.clone())))
        {
            return Err(Error::PermissionDenied(
                "Ancilo's own data is not read as a document".into(),
            ));
        }
        let name = file.file_name().unwrap_or_default();
        let inside = workdir.join(name);
        if !inside.exists() {
            std::fs::copy(file, &inside).map_err(|e| {
                Error::invalid(format!("cannot read {}: {e}", name.to_string_lossy()))
            })?;
        }
        Ok(inside)
    }

    async fn read_apart(&self, bin: &Path, file: &Path, workdir: &Path) -> Result<Extracted> {
        let out = self
            .run_apart(bin, "extract-document", file, workdir)
            .await?;
        serde_json::from_slice(&out).map_err(Error::internal)
    }

    /// What a file looks like inside – for looking at a result before
    /// keeping it ([`preview`]) – read like a document: apart, sandboxed,
    /// with a time limit. `workdir` is removed afterwards.
    pub async fn layout(&self, file: &Path, workdir: &Path) -> Result<preview::Layout> {
        let result = async {
            match &self.bin {
                Some(bin) => {
                    let file = self.inside(file, workdir)?;
                    let out = self
                        .run_apart(bin, "preview-document", &file, workdir)
                        .await?;
                    serde_json::from_slice(&out).map_err(Error::internal)
                }
                None => {
                    let file = file.to_path_buf();
                    tokio::task::spawn_blocking(move || {
                        std::panic::catch_unwind(|| preview::layout_file(&file))
                            .unwrap_or_else(|_| Err(Error::invalid("this file could not be read")))
                    })
                    .await
                    .map_err(Error::internal)?
                }
            }
        }
        .await;
        std::fs::remove_dir_all(workdir).ok();
        result
    }

    /// Runs the reader (`what`: `extract-document`, `preview-document`) on
    /// `file`; its output.
    async fn run_apart(
        &self,
        bin: &Path,
        what: &str,
        file: &Path,
        workdir: &Path,
    ) -> Result<Vec<u8>> {
        let mut cmd = reader_command(bin, what, file, workdir, &self.hidden)?;
        cmd.current_dir(workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(Error::internal)?;
        let out = match tokio::time::timeout(READ_TIMEOUT, child.wait_with_output()).await {
            Err(_) => {
                return Err(Error::invalid(ancilo_core::msg("doc.too_long", &[])));
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
        Ok(out.stdout)
    }
}

/// The reading process: on macOS in a sandbox that allows nothing but
/// running the reader, the system's libraries and its own place – no
/// network, no other file of the user. Without a sandbox nothing is read.
#[cfg(target_os = "macos")]
fn reader_command(
    bin: &Path,
    what: &str,
    file: &Path,
    workdir: &Path,
    _hidden: &[PathBuf],
) -> Result<tokio::process::Command> {
    let q = |p: &Path| p.display().to_string().replace(['"', '\\'], "");
    let bin_real = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    let profile = format!(
        r#"(version 1)
(deny default)
(allow process-exec (literal "{bin}") (literal "{bin_real}"))
(allow file-read* (literal "/") (literal "{bin}") (literal "{bin_real}") (subpath "{work}")
  (subpath "/usr/lib") (subpath "/usr/share") (subpath "/System") (subpath "/private/var/db/dyld")
  (subpath "/Library/Apple") (literal "/dev/null") (literal "/dev/urandom") (literal "/dev/random")
  (literal "/private/etc/localtime"))
(allow file-read-metadata)
(allow file-write* (subpath "{work}") (literal "/dev/null"))
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.system.logger"))
"#,
        bin = q(bin),
        bin_real = q(&bin_real),
        work = q(workdir),
    );
    let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
    c.arg("-p")
        .arg(profile)
        .arg(bin)
        .arg(what)
        .arg(file)
        .env_clear()
        .env("LANG", "C.UTF-8");
    Ok(c)
}

#[cfg(not(target_os = "macos"))]
fn reader_command(
    bin: &Path,
    what: &str,
    file: &Path,
    workdir: &Path,
    hidden: &[PathBuf],
) -> Result<tokio::process::Command> {
    let script = format!(
        "exec {} {what} {}",
        quote(&bin.display().to_string()),
        quote(&file.display().to_string())
    );
    let bounds = ancilo_agent::sandbox::Bounds {
        root: workdir,
        hidden,
        network: false,
    };
    ancilo_agent::sandbox::command(&bounds, &script).map_err(|_| {
        Error::unavailable(
            "documents cannot be read safely on this system – install bubblewrap (bwrap)",
        )
    })
}

#[cfg(not(target_os = "macos"))]
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
    pub fn new(db: Db, extractor: Arc<Extractor>) -> Self {
        Self { db, extractor }
    }

    /// Reads a document the app sent (dragged in or chosen).
    pub async fn add_bytes(&self, name: &str, bytes: &[u8]) -> Result<AttachmentView> {
        let name = file_name(name);
        if extract::kind_of(&name).is_none() {
            // Says which kinds Ancilo reads.
            extract::extract(&name, b"")?;
        }
        if bytes.len() as u64 > extract::MAX_BYTES {
            return Err(Error::invalid(ancilo_core::msg(
                "doc.too_large",
                &[("name", &name), ("mb", &(extract::MAX_BYTES / 1024 / 1024))],
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

    /// The names of attached documents without any text (a picture without
    /// recognizable text, a scan that could not be read) – the model must
    /// say so instead of guessing.
    pub fn without_text(&self, ids: &[String]) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for id in ids {
            if self.parts(id)?.iter().all(|p| p.text.trim().is_empty()) {
                names.push(self.view(id)?.name);
            }
        }
        Ok(names)
    }

    /// The text for a question: everything when the documents are short,
    /// else the passages that fit `query` best – in document order, each with
    /// its source (`[file, page 3]`).
    pub fn passages(&self, ids: &[String], query: &str) -> Result<Vec<String>> {
        let mut docs = Vec::new();
        for id in ids {
            docs.push((self.view(id)?.name, self.parts(id)?));
        }
        Ok(choose(&docs, query, PASSAGE_BUDGET))
    }

    /// The passages for a question as evidence (marks given by the caller).
    pub fn evidence(&self, ids: &[String], query: &str) -> Result<Vec<evidence::Evidence>> {
        // Chosen by the attachment's id: two attachments may share a name.
        let mut docs = Vec::new();
        let mut about = std::collections::HashMap::new();
        for id in ids {
            let view = self.view(id)?;
            let parts = self.parts(id)?;
            about.insert(
                id.clone(),
                evidence::About {
                    document: view.name.clone(),
                    origin: Some(evidence::Origin::Attachment { id: id.clone() }),
                    revision: evidence::revision(&parts),
                    warnings: view.warnings.clone(),
                    file: None,
                },
            );
            docs.push((id.clone(), parts));
        }
        let passages = select(&docs, query, PASSAGE_BUDGET, SEGMENTER);
        Ok(evidence::of(passages, |id| {
            about.get(id).cloned().unwrap_or_default()
        }))
    }

    /// An attachment's text now (`None`: deleted).
    pub fn current(&self, id: &str) -> Option<Vec<Part>> {
        self.parts(id).ok()
    }

    /// The reader, for a [`library::Library`] to share.
    pub fn extractor(&self) -> Arc<Extractor> {
        self.extractor.clone()
    }
}

/// How a document's text is cut into passages to choose from, and what
/// besides their words finds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segmenter {
    pub cut: Cut,
    pub header: Header,
}

/// Where passages are cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// Paragraphs joined to about 700 characters (Ancilo's way, as web
    /// pages are cut – `ancilo_web::rank::passages`), kept line by line.
    Paragraphs,
    /// LangChain's recursive splitter as AnythingLLM uses it ([`split`]).
    Recursive { size: usize, overlap: usize },
}

/// How a passage's document name and page or sheet count when choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// Not at all.
    None,
    /// AnythingLLM: written before every chunk – each word of the name
    /// counts in every passage of the document.
    Prepend,
    /// Ancilo: the names are scored on their own (a word all names share
    /// counts little) and lift only passages that fit by their own words.
    Boost,
}

/// A passage chosen for a question, and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Passage {
    pub document: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Locator>,
    /// Which part of the document (page, sheet) – by position.
    pub part: usize,
    /// Where it starts in that part's text (characters) – when it is the
    /// text as it stands there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    pub text: String,
}

/// How much a fitting name lifts a passage, against its own words.
const HEADER_WEIGHT: f64 = 0.5;

/// What a passage is found by, besides its text: its document's name and
/// where in it (`Verträge/Miete 2025.pdf`, sheet `2024` → words to match).
fn header(name: &str, at: Option<&Locator>) -> String {
    let words: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    match at {
        Some(Locator::Page(n)) => format!("{words} page {n}"),
        Some(Locator::Sheet(s)) => format!("{words} sheet {s}"),
        None => words,
    }
}

/// The passages of documents (name, parts) for a question: all of it when
/// it fits `budget`, else the passages that fit `query` best, in document
/// order.
pub fn select(
    docs: &[(String, Vec<Part>)],
    query: &str,
    budget: usize,
    seg: Segmenter,
) -> Vec<Passage> {
    let whole: Vec<Passage> = docs
        .iter()
        .flat_map(|(name, parts)| {
            parts.iter().enumerate().map(move |(i, p)| Passage {
                document: name.clone(),
                at: p.at.clone(),
                part: i,
                start: Some(p.text.chars().take_while(|c| c.is_whitespace()).count()),
                text: p.text.trim().to_string(),
            })
        })
        .collect();
    let total: usize = whole.iter().map(|p| p.text.chars().count()).sum();
    if total <= budget {
        return whole.into_iter().filter(|p| !p.text.is_empty()).collect();
    }
    let mut picked: Vec<(usize, Passage)> = Vec::new();
    let mut used = 0;
    for (i, p) in ranked(docs, query, seg) {
        let n = p.text.chars().count();
        if used + n > budget {
            continue;
        }
        used += n;
        picked.push((i, p));
    }
    picked.sort_by_key(|(i, _)| *i);
    picked.into_iter().map(|(_, p)| p).collect()
}

/// All passages of the documents, best for `query` first (with the
/// position each has in the documents) – where [`select`] takes them from.
pub fn ranked(docs: &[(String, Vec<Part>)], query: &str, seg: Segmenter) -> Vec<(usize, Passage)> {
    let mut chunks: Vec<Passage> = Vec::new();
    let mut found_by: Vec<String> = Vec::new();
    let mut heads: Vec<String> = Vec::new();
    for (name, parts) in docs {
        for (i, p) in parts.iter().enumerate() {
            let head = header(name, p.at.as_ref());
            let cuts: Vec<(Option<usize>, String)> = match seg.cut {
                Cut::Paragraphs => paragraphs(&p.text),
                Cut::Recursive { size, overlap } => split::Splitter::new(size, overlap)
                    .chunks(&p.text)
                    .into_iter()
                    .map(|c| (Some(c.start), c.text))
                    .collect(),
            };
            for (start, text) in cuts {
                found_by.push(if seg.header == Header::Prepend {
                    format!("{head}\n{text}")
                } else {
                    text.clone()
                });
                heads.push(head.clone());
                chunks.push(Passage {
                    document: name.clone(),
                    at: p.at.clone(),
                    part: i,
                    start,
                    text,
                });
            }
        }
    }
    let mut scores = ancilo_web::rank::scores(&found_by, query);
    if seg.header == Header::Boost {
        // The names, each once: how well each fits the question – a word
        // in every name ("pdf", "Angebot" in all offers) counts little.
        let mut names: Vec<String> = heads.clone();
        names.sort();
        names.dedup();
        let fit = ancilo_web::rank::scores(&names, query);
        for (i, h) in heads.iter().enumerate() {
            if scores[i] > 0.0
                && let Ok(n) = names.binary_search(h)
            {
                scores[i] += HEADER_WEIGHT * fit[n];
            }
        }
    }
    let mut order: Vec<usize> = (0..chunks.len()).filter(|i| scores[*i] > 0.0).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then(a.cmp(b)));
    // Nothing matches the words of the question ("summarise this"): the
    // beginning of every document, in turn.
    if order.is_empty() {
        let mut firsts: Vec<usize> = Vec::new();
        let mut seen = HashSet::new();
        for (i, c) in chunks.iter().enumerate() {
            if seen.insert(c.document.clone()) {
                firsts.push(i);
            }
        }
        let rest = (0..chunks.len()).filter(|i| !firsts.contains(i));
        order = firsts.iter().copied().chain(rest).collect();
    }
    order.into_iter().map(|i| (i, chunks[i].clone())).collect()
}

/// How Ancilo cuts documents now (FPL-01, `evals/fpl01`): its paragraphs –
/// the recursive splitter was measured and found no better.
pub const SEGMENTER: Segmenter = Segmenter {
    cut: Cut::Paragraphs,
    header: Header::None,
};

/// Ancilo's paragraph passages (`ancilo_web::rank::passages`: the same
/// cuts, the same lengths) – with their lines kept and where each starts in
/// the text, so a source can show the passage as it stands there.
fn paragraphs(text: &str) -> Vec<(Option<usize>, String)> {
    let joined = ancilo_web::rank::passages_by_line(text);
    // Where each starts: its first line, searched on from the last one.
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = 0usize;
    joined
        .into_iter()
        .map(|p| {
            let first: Vec<char> = p.split('\n').next().unwrap_or_default().chars().collect();
            let at = (cursor..=chars.len().saturating_sub(first.len()))
                .find(|&i| !first.is_empty() && chars[i..i + first.len()] == first[..]);
            if let Some(a) = at {
                cursor = a + 1;
            }
            (at, p)
        })
        .collect()
}

/// Text of documents (name, parts) for a question ([`select`]), each
/// passage headed by its source.
pub fn choose(docs: &[(String, Vec<Part>)], query: &str, budget: usize) -> Vec<String> {
    select(docs, query, budget, SEGMENTER)
        .into_iter()
        .map(|p| format!("{}\n{}", source(&p.document, p.at.as_ref()), p.text))
        .collect()
}

/// Where a passage comes from, as the model is asked to cite it.
pub(crate) fn source(name: &str, at: Option<&Locator>) -> String {
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
        (dir, Attachments::new(db, Arc::new(ex)))
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

    // covers: FPL-01 – two attachments of the same name keep their own
    // origin and revision (review 1, finding 1).
    #[tokio::test]
    async fn attachments_of_the_same_name_keep_their_own_sources() {
        let (_d, a) = store();
        let first = a
            .add_bytes("same.txt", b"FIRST UNIQUE words")
            .await
            .unwrap();
        let second = a
            .add_bytes("same.txt", b"SECOND UNIQUE words")
            .await
            .unwrap();
        let e = a
            .evidence(&[first.id.clone(), second.id.clone()], "unique")
            .unwrap();
        assert_eq!(e.len(), 2);
        for (ev, id, text) in [(&e[0], &first.id, "FIRST"), (&e[1], &second.id, "SECOND")] {
            assert!(ev.text.contains(text), "{ev:?}");
            assert_eq!(ev.origin, evidence::Origin::Attachment { id: id.clone() });
            assert_eq!(ev.document, "same.txt");
            assert_eq!(ev.revision, evidence::revision(&a.current(id).unwrap()));
        }
        assert_ne!(e[0].revision, e[1].revision);
    }

    #[tokio::test]
    async fn what_cannot_be_read_is_refused_with_the_reason() {
        let (_d, a) = store();
        let e = a.add_bytes("x.exe", b"MZ").await.unwrap_err();
        assert!(e.message().contains("reads PDF, Word"), "{}", e.message());
        let e = a.add_path(Path::new("relative.pdf")).await.unwrap_err();
        assert!(e.message().contains("full path"));
    }

    // covers: M10-AC-02
    /// A photo of a receipt and a scanned PDF get their text – recognized on
    /// this computer, in the sandbox, marked as recognized.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn pictures_and_scans_get_their_text_recognized() {
        let Some(helper) = ocr::helper(None) else {
            eprintln!("no text recognition helper in this build (swiftc missing)");
            return;
        };
        let t = tempfile::tempdir().unwrap();
        // As in the app: the helper inside an app bundle (Foundation reads the
        // bundle – in a sandbox that forgot it, Vision found nothing).
        let macos = t.path().join("Ancilo.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(
            macos.parent().unwrap().join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>app.ancilo.test</string><key>CFBundleExecutable</key><string>ancilo-app</string></dict></plist>"#,
        )
        .unwrap();
        std::fs::copy(&helper, macos.join("ancilo-ocr")).unwrap();
        let helper = macos.join("ancilo-ocr");
        let ex = Extractor::new(None, t.path().join("scratch"), Vec::new()).with_ocr(Some(helper));
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let photo = ex
            .read(&fixtures.join("receipt.jpg"), &ex.workdir().unwrap())
            .await
            .unwrap();
        assert_eq!(photo.kind, Kind::Image);
        assert_eq!(photo.warnings, [Warning::Recognized]);
        assert_eq!(photo.parts.len(), 1);
        assert!(
            photo.parts[0].text.contains("Summe 6,30 EUR"),
            "{:?}",
            photo.parts
        );
        assert_eq!(photo.parts[0].at, None);
        let scan = ex
            .read(&fixtures.join("scan.pdf"), &ex.workdir().unwrap())
            .await
            .unwrap();
        assert_eq!(scan.kind, Kind::Pdf);
        assert_eq!(scan.warnings, [Warning::Recognized]);
        assert_eq!(scan.parts[0].at, Some(Locator::Page(1)));
        assert!(
            scan.parts[0].text.contains("Datum 21.10.2022"),
            "{:?}",
            scan.parts
        );
        // Without recognition a picture is a document without text.
        let plain = Extractor::new(None, t.path().join("scratch2"), Vec::new());
        let photo = plain
            .read(&fixtures.join("receipt.jpg"), &plain.workdir().unwrap())
            .await
            .unwrap();
        assert!(photo.parts.is_empty() && photo.warnings == [Warning::NoText]);
    }
}
