//! The coding tools: read, write, edit, grep, glob, bash.
//!
//! All paths are confined to the workspace root. Writes require the `edit`
//! permission, commands the `shell` permission (and run in a sandbox). A file
//! that changed on disk since the agent read it cannot be overwritten or
//! edited – the agent has to read it again (protection against concurrent
//! changes by the calling agent or the user).

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use ancilo_core::EventBus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::sandbox;

/// What the agent may do in the workspace.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// read_file, grep, glob
    Read,
    /// + write_file, edit_file (inside the workspace only)
    Edit,
    /// + bash (sandboxed: no network, writes only inside the workspace)
    Shell,
}

/// Result of one tool call, as sent back to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// What an agent can do: tool definitions and their execution. A coding
/// workspace is one toolbox; the assistant's registry operations another.
pub trait Toolbox: Send + Sync {
    /// OpenAI tool definitions.
    fn definitions(&self) -> Vec<Value>;
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
    ) -> ancilo_core::BoxFuture<'a, ToolOutput>;
    /// Files changed so far and their unified diff.
    fn changes(&self) -> (Vec<FileChange>, String) {
        (Vec::new(), String::new())
    }
    /// Added to the task (e.g. the project root and permissions).
    fn context(&self) -> Option<String> {
        None
    }
}

impl Toolbox for Workspace {
    fn definitions(&self) -> Vec<Value> {
        Workspace::definitions(self)
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
    ) -> ancilo_core::BoxFuture<'a, ToolOutput> {
        Box::pin(Workspace::execute(self, name, args))
    }
    fn changes(&self) -> (Vec<FileChange>, String) {
        Workspace::changes(self)
    }
    fn context(&self) -> Option<String> {
        Some(format!(
            "(Project root: {} · allowed: {:?})",
            self.shown_root().display(),
            self.access()
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileChange {
    pub path: String,
    /// `created` or `modified`
    pub kind: String,
    pub added: usize,
    pub removed: usize,
}

/// Settings for the `bash` tool.
#[derive(Debug, Clone)]
pub struct ShellSettings {
    pub sandbox: bool,
    pub network: bool,
    /// Places commands may not read, besides the usual secret places
    /// (Ancilo's home, with other sessions and its key file).
    pub hidden: Vec<PathBuf>,
    pub default_timeout: Duration,
    pub max_timeout: Duration,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            sandbox: true,
            network: false,
            hidden: Vec::new(),
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
        }
    }
}

/// The directory an agent works in, with its permissions and bookkeeping.
pub struct Workspace {
    root: PathBuf,
    access: Access,
    shell: ShellSettings,
    /// SHA-256 of each file as the agent last saw it.
    seen: Mutex<HashMap<PathBuf, String>>,
    /// Content before the first change (`None`: the file did not exist).
    originals: Mutex<BTreeMap<PathBuf, Option<String>>>,
    bus: Option<(EventBus, String)>,
    search: Option<std::sync::Arc<dyn CodeSearch>>,
    /// The folder the agent and the user know the project by, when the agent
    /// works in a copy of it (a session's work area): the model only ever
    /// sees this path, and paths under it lead into the copy.
    shown: Option<PathBuf>,
}

/// Replaces the path `from` in `text` with `to` where it stands as a whole
/// path (not inside a longer name like `/a/wcs2` or `/x/a/wcs`).
fn replace_path(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() || from == to {
        return text.to_string();
    }
    let name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut prev: Option<char> = None;
    while let Some(i) = rest.find(from) {
        let before = rest[..i]
            .chars()
            .next_back()
            .or(if i == 0 { prev } else { None });
        let after = &rest[i + from.len()..];
        let mut next = after.chars();
        let ends = match next.next() {
            None => true,
            Some('.') => !next.next().is_some_and(name_char),
            Some(c) => !name_char(c),
        };
        let starts = !before.is_some_and(|c| name_char(c) || c == '.');
        out.push_str(&rest[..i]);
        out.push_str(if starts && ends { to } else { from });
        prev = from.chars().next_back();
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Search over the project (the index of M5), offered to the model as the
/// tool `search` when available.
pub trait CodeSearch: Send + Sync {
    /// Returns the hits as text for the model.
    fn search(
        &self,
        root: PathBuf,
        query: String,
        limit: usize,
    ) -> ancilo_core::BoxFuture<'static, Result<String, String>>;
    /// Starts indexing `root` in the background (e.g. when a task begins).
    fn prepare(&self, root: PathBuf);
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
}

const READ_DEFAULT_LINES: usize = 400;
const MAX_LINE_CHARS: usize = 500;
const MAX_MATCHES: usize = 100;
const MAX_OUTPUT_CHARS: usize = 20_000;

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() > MAX_LINE_CHARS {
        format!("{}…", line.chars().take(MAX_LINE_CHARS).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Keeps head and tail of long outputs.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max / 2).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(max / 2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n… ({} characters omitted) …\n{tail}",
        text.len() - max
    )
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    timeout_s: Option<u64>,
}

impl Workspace {
    pub fn new(root: &Path, access: Access) -> std::io::Result<Self> {
        Ok(Self {
            root: std::fs::canonicalize(root)?,
            access,
            shell: ShellSettings::default(),
            seen: Mutex::new(HashMap::new()),
            originals: Mutex::new(BTreeMap::new()),
            bus: None,
            search: None,
            shown: None,
        })
    }

    /// The agent works in a copy of `project`: it sees and uses the project's
    /// path; everything still happens in this workspace.
    pub fn showing(mut self, project: &Path) -> Self {
        let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        if project != self.root {
            self.shown = Some(project);
        }
        self
    }

    /// The project path the agent sees.
    pub fn shown_root(&self) -> &Path {
        self.shown.as_deref().unwrap_or(&self.root)
    }

    /// Paths the agent wrote → paths in this workspace.
    fn inbound(&self, text: &str) -> String {
        match &self.shown {
            Some(shown) => replace_path(
                text,
                &shown.display().to_string(),
                &self.root.display().to_string(),
            ),
            None => text.to_string(),
        }
    }

    /// Paths in this workspace → the paths the agent knows.
    fn outbound(&self, text: &str) -> String {
        let Some(shown) = &self.shown else {
            return text.to_string();
        };
        let shown = shown.display().to_string();
        let root = self.root.display().to_string();
        let text = replace_path(text, &root, &shown);
        // The same folder without the /private prefix of macOS temp paths.
        match root.strip_prefix("/private") {
            Some(short) if short.starts_with('/') => replace_path(&text, short, &shown),
            _ => text,
        }
    }

    /// Offers the `search` tool backed by this provider.
    pub fn with_search(mut self, search: std::sync::Arc<dyn CodeSearch>) -> Self {
        search.prepare(self.root.clone());
        self.search = Some(search);
        self
    }

    pub fn with_events(mut self, bus: EventBus, subject: &str) -> Self {
        self.bus = Some((bus, subject.to_string()));
        self
    }

    pub fn with_shell(mut self, shell: ShellSettings) -> Self {
        self.shell = shell;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn access(&self) -> Access {
        self.access
    }

    fn emit(&self, kind: &str, data: Value) {
        if let Some((bus, subject)) = &self.bus {
            bus.emit(kind, Some(subject), data);
        }
    }

    /// Resolves a path inside the workspace; refuses anything outside.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let inside = self.inbound(path.trim());
        let p = Path::new(&inside);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let mut normal = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::ParentDir => {
                    normal.pop();
                }
                Component::CurDir => {}
                other => normal.push(other.as_os_str()),
            }
        }
        // Resolve symlinks of the existing part of the path.
        let mut existing = normal.clone();
        let mut rest = Vec::new();
        while !existing.exists() {
            match (
                existing.file_name().map(|n| n.to_os_string()),
                existing.parent(),
            ) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    existing = parent.to_path_buf();
                }
                _ => break,
            }
        }
        let mut real = std::fs::canonicalize(&existing).unwrap_or(existing);
        for name in rest.into_iter().rev() {
            real.push(name);
        }
        if !real.starts_with(&self.root) {
            return Err(format!(
                "'{path}' is outside the project ({}) – access denied",
                self.shown_root().display()
            ));
        }
        Ok(real)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .display()
            .to_string()
    }

    /// OpenAI tool definitions for this workspace's permission level.
    pub fn definitions(&self) -> Vec<Value> {
        let f = |name: &str, desc: &str, params: Value| json!({"type": "function", "function": {"name": name, "description": desc, "parameters": params}});
        let mut tools = vec![
            f(
                "read_file",
                "Read a text file (numbered lines). Use offset/limit for long files.",
                json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "Relative to the project root"},
                "offset": {"type": "integer", "minimum": 1, "description": "First line (1-based)"},
                "limit": {"type": "integer", "minimum": 1, "description": "Number of lines"}}, "required": ["path"]}),
            ),
            f(
                "grep",
                "Search file contents with a regular expression. Returns path:line: text.",
                json!({"type": "object", "properties": {
                "pattern": {"type": "string"}, "path": {"type": "string", "description": "Only this file or directory"},
                "glob": {"type": "string", "description": "Only files matching, e.g. \"*.rs\""},
                "case_insensitive": {"type": "boolean"}}, "required": ["pattern"]}),
            ),
            f(
                "glob",
                "Find files by name pattern, e.g. \"src/**/*.ts\" or \"*.md\".",
                json!({"type": "object", "properties": {
                "pattern": {"type": "string"}}, "required": ["pattern"]}),
            ),
        ];
        if self.search.is_some() {
            tools.insert(0, f(
                "search",
                "Find the code relevant to a question or name (meaning and exact words). Returns path:lines, symbol and a short preview per hit – then read the lines you need. Use it first when you do not know where something is.",
                json!({"type": "object", "properties": {
                "query": {"type": "string", "description": "e.g. \"where is the token checked\" or \"parse_config\""},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20}}, "required": ["query"]}),
            ));
        }
        if self.access >= Access::Edit {
            tools.push(f("edit_file", "Replace an exact snippet of a file with new text. old_text must match the file (include enough context to be unique).", json!({"type": "object", "properties": {
                "path": {"type": "string"}, "old_text": {"type": "string"}, "new_text": {"type": "string"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence"}}, "required": ["path", "old_text", "new_text"]})));
            tools.push(f("write_file", "Create a file, or overwrite a file you have read, with the complete content.", json!({"type": "object", "properties": {
                "path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]})));
        }
        if self.access >= Access::Shell {
            tools.push(f("bash", "Run a shell command in the project directory (no network). Returns output and exit code.", json!({"type": "object", "properties": {
                "command": {"type": "string"}, "timeout_s": {"type": "integer", "minimum": 1, "maximum": 600}}, "required": ["command"]})));
        }
        tools
    }

    /// Executes a tool call. Errors are returned to the model, not raised.
    pub async fn execute(&self, name: &str, args: &Value) -> ToolOutput {
        let mut out = self.run_tool(name, args).await;
        out.content = self.outbound(&out.content);
        out
    }

    async fn run_tool(&self, name: &str, args: &Value) -> ToolOutput {
        let parse = |v: &Value| -> Result<Value, String> { Ok(v.clone()) };
        let args = match parse(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        macro_rules! typed {
            ($t:ty) => {
                match serde_json::from_value::<$t>(args.clone()) {
                    Ok(a) => a,
                    Err(e) => return ToolOutput::err(format!("invalid arguments for {name}: {e}")),
                }
            };
        }
        match name {
            "read_file" => self.read(typed!(ReadArgs)),
            "write_file" if self.access >= Access::Edit => self.write(typed!(WriteArgs)),
            "edit_file" if self.access >= Access::Edit => self.edit(typed!(EditArgs)),
            "grep" => self.grep(typed!(GrepArgs)),
            "glob" => self.glob(typed!(GlobArgs)),
            "search" if self.search.is_some() => {
                let a = typed!(SearchArgs);
                let s = self.search.clone().unwrap();
                match s
                    .search(
                        self.root.clone(),
                        a.query,
                        a.limit.unwrap_or(5).clamp(1, 20),
                    )
                    .await
                {
                    Ok(text) => ToolOutput::ok(clip(&text, MAX_OUTPUT_CHARS)),
                    Err(e) => ToolOutput::err(format!("search failed: {e}")),
                }
            }
            "bash" if self.access >= Access::Shell => self.bash(typed!(BashArgs)).await,
            "write_file" | "edit_file" | "bash" => ToolOutput::err(format!(
                "'{name}' is not allowed with this task's permission ({:?})",
                self.access
            )),
            other => ToolOutput::err(format!("unknown tool '{other}'")),
        }
    }

    fn read(&self, a: ReadArgs) -> ToolOutput {
        let path = match self.resolve(&a.path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let text = match std::fs::read(&path) {
            Ok(bytes) if bytes.iter().take(8000).any(|b| *b == 0) => {
                return ToolOutput::err(format!("{} is a binary file", a.path));
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if path.is_dir() => {
                return ToolOutput::err(format!(
                    "{} is a directory – use glob to list files ({e})",
                    a.path
                ));
            }
            Err(e) => return ToolOutput::err(format!("cannot read {}: {e}", a.path)),
        };
        self.seen.lock().unwrap().insert(path.clone(), sha(&text));
        let lines: Vec<&str> = text.lines().collect();
        let start = a.offset.unwrap_or(1).max(1);
        let limit = a.limit.unwrap_or(READ_DEFAULT_LINES).max(1);
        if lines.is_empty() {
            return ToolOutput::ok(format!("{} is empty", a.path));
        }
        if start > lines.len() {
            return ToolOutput::err(format!("{} has only {} lines", a.path, lines.len()));
        }
        let end = (start - 1 + limit).min(lines.len());
        let mut out: String = lines[start - 1..end]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{:>6}\t{}", start + i, truncate_line(l)))
            .collect::<Vec<_>>()
            .join("\n");
        if end < lines.len() {
            out.push_str(&format!(
                "\n… {} more lines (read with offset={})",
                lines.len() - end,
                end + 1
            ));
        }
        ToolOutput::ok(out)
    }

    /// Refuses to change a file that changed since the agent read it.
    fn check_fresh(&self, path: &Path, current: Option<&str>) -> Result<(), String> {
        let seen = self.seen.lock().unwrap().get(path).cloned();
        match (seen, current) {
            (Some(h), Some(c)) if h != sha(c) => Err(format!(
                "{} was changed by someone else since you read it – read it again before changing it",
                self.rel(path)
            )),
            _ => Ok(()),
        }
    }

    fn commit_change(
        &self,
        path: &Path,
        before: Option<String>,
        after: &str,
    ) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create directory: {e}"))?;
        }
        std::fs::write(path, after).map_err(|e| format!("cannot write {}: {e}", self.rel(path)))?;
        let created = before.is_none();
        self.originals
            .lock()
            .unwrap()
            .entry(path.to_path_buf())
            .or_insert(before);
        self.seen
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), sha(after));
        self.emit(
            "agent.file_changed",
            json!({"path": self.rel(path), "kind": if created { "created" } else { "modified" }}),
        );
        Ok(())
    }

    fn write(&self, a: WriteArgs) -> ToolOutput {
        let path = match self.resolve(&a.path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let current = std::fs::read_to_string(&path).ok();
        if current.is_some() && !self.seen.lock().unwrap().contains_key(&path) {
            return ToolOutput::err(format!(
                "{} exists – read it first, then write the complete new content (or use edit_file)",
                a.path
            ));
        }
        if let Err(e) = self.check_fresh(&path, current.as_deref()) {
            return ToolOutput::err(e);
        }
        match self.commit_change(&path, current.clone(), &a.content) {
            Ok(()) => ToolOutput::ok(format!(
                "{} {} ({} lines)",
                if current.is_some() {
                    "wrote"
                } else {
                    "created"
                },
                a.path,
                a.content.lines().count()
            )),
            Err(e) => ToolOutput::err(e),
        }
    }

    fn edit(&self, a: EditArgs) -> ToolOutput {
        let path = match self.resolve(&a.path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let current = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                return ToolOutput::err(format!(
                    "{} does not exist – use write_file to create it",
                    a.path
                ));
            }
        };
        if let Err(e) = self.check_fresh(&path, Some(&current)) {
            return ToolOutput::err(e);
        }
        if a.old_text.is_empty() {
            return ToolOutput::err(
                "old_text is empty – quote the text to replace, or use write_file for the whole file",
            );
        }
        let updated = match replace(&current, &a.old_text, &a.new_text, a.replace_all) {
            Ok(u) => u,
            Err(e) => return ToolOutput::err(format!("{}: {e}", a.path)),
        };
        match self.commit_change(&path, Some(current), &updated.text) {
            Ok(()) => ToolOutput::ok(format!(
                "edited {} ({} replacement{}{})",
                a.path,
                updated.count,
                if updated.count == 1 { "" } else { "s" },
                if updated.fuzzy {
                    ", matched ignoring indentation"
                } else {
                    ""
                }
            )),
            Err(e) => ToolOutput::err(e),
        }
    }

    fn walker(&self, start: &Path) -> ignore::Walk {
        ignore::WalkBuilder::new(start)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build()
    }

    fn grep(&self, a: GrepArgs) -> ToolOutput {
        let start = match a.path.as_deref().map(|p| self.resolve(p)).transpose() {
            Ok(p) => p.unwrap_or_else(|| self.root.clone()),
            Err(e) => return ToolOutput::err(e),
        };
        let re = regex::RegexBuilder::new(&a.pattern)
            .case_insensitive(a.case_insensitive)
            .build()
            .or_else(|_| {
                regex::RegexBuilder::new(&regex::escape(&a.pattern))
                    .case_insensitive(a.case_insensitive)
                    .build()
            });
        let re = match re {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("invalid pattern: {e}")),
        };
        let filter = a
            .glob
            .as_deref()
            .and_then(|g| {
                globset::GlobBuilder::new(g)
                    .literal_separator(false)
                    .build()
                    .ok()
            })
            .map(|g| g.compile_matcher());
        let mut hits = Vec::new();
        let mut truncated = false;
        'files: for entry in self.walker(&start).flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let rel = self.rel(p);
            if let Some(f) = &filter
                && !f.is_match(&rel)
                && !p.file_name().is_some_and(|n| f.is_match(n))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(p) else {
                continue;
            };
            if bytes.iter().take(8000).any(|b| *b == 0) {
                continue;
            }
            for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                if re.is_match(line) {
                    if hits.len() >= MAX_MATCHES {
                        truncated = true;
                        break 'files;
                    }
                    hits.push(format!(
                        "{rel}:{}: {}",
                        i + 1,
                        truncate_line(line.trim_end())
                    ));
                }
            }
        }
        if hits.is_empty() {
            return ToolOutput::ok(format!("no matches for {:?}", a.pattern));
        }
        let mut out = hits.join("\n");
        if truncated {
            out.push_str(&format!(
                "\n… more than {MAX_MATCHES} matches – narrow the search"
            ));
        }
        ToolOutput::ok(out)
    }

    fn glob(&self, a: GlobArgs) -> ToolOutput {
        let pattern = a.pattern.trim().trim_start_matches("./");
        let glob = match globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("invalid pattern: {e}")),
        };
        let mut found: Vec<String> = self
            .walker(&self.root)
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| self.rel(e.path()))
            .filter(|rel| {
                glob.is_match(rel) || Path::new(rel).file_name().is_some_and(|n| glob.is_match(n))
            })
            .collect();
        found.sort();
        if found.is_empty() {
            return ToolOutput::ok(format!("no files match {pattern:?}"));
        }
        let total = found.len();
        found.truncate(200);
        let mut out = found.join("\n");
        if total > 200 {
            out.push_str(&format!("\n… {} more", total - 200));
        }
        ToolOutput::ok(out)
    }

    async fn bash(&self, a: BashArgs) -> ToolOutput {
        let timeout = a
            .timeout_s
            .map(Duration::from_secs)
            .unwrap_or(self.shell.default_timeout)
            .min(self.shell.max_timeout);
        let script = self.inbound(&a.command);
        let mut cmd = if self.shell.sandbox {
            let bounds = sandbox::Bounds {
                root: &self.root,
                hidden: &self.shell.hidden,
                network: self.shell.network,
            };
            match sandbox::command(&bounds, &script) {
                Ok(c) => c,
                Err(e) => return ToolOutput::err(e),
            }
        } else {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(&script);
            c
        };
        cmd.current_dir(&self.root)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        // Its own process group: a timeout, a cancelled turn or the end of the
        // command stops everything it started, not only the shell.
        #[cfg(unix)]
        cmd.process_group(0);
        self.emit("agent.command", json!({"command": a.command}));
        let mut child = match cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("cannot run command: {e}")),
        };
        let group = ProcessGroup(child.id());
        let collect = |pipe: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = tokio::io::AsyncReadExt::read_to_end(&mut p, &mut buf).await;
                }
                buf
            })
        };
        let stdout = collect(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = collect(child.stderr.take().map(|p| Box::new(p) as _));
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Err(_) => {
                return ToolOutput::err(format!("command timed out after {}s", timeout.as_secs()));
            }
            Ok(Err(e)) => return ToolOutput::err(format!("cannot run command: {e}")),
            Ok(Ok(s)) => s,
        };
        // The command is done: what it left running in the background stops,
        // which also closes the output pipes.
        drop(group);
        let out = std::process::Output {
            status,
            stdout: stdout.await.unwrap_or_default(),
            stderr: stderr.await.unwrap_or_default(),
        };
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() {
            text.push_str(&err);
        }
        let code = out.status.code().unwrap_or(-1);
        let body = format!(
            "{}\n[exit code {code}]",
            clip(text.trim_end(), MAX_OUTPUT_CHARS)
        );
        if out.status.success() {
            ToolOutput::ok(body)
        } else {
            ToolOutput::err(body)
        }
    }

    /// What changed, with line counts and a unified diff.
    pub fn changes(&self) -> (Vec<FileChange>, String) {
        let originals = self.originals.lock().unwrap().clone();
        let mut changes = Vec::new();
        let mut diff = String::new();
        for (path, before) in originals {
            let after = std::fs::read_to_string(&path).unwrap_or_default();
            let old = before.clone().unwrap_or_default();
            if before.as_deref() == Some(after.as_str()) {
                continue;
            }
            let d = similar::TextDiff::from_lines(&old, &after);
            let (mut added, mut removed) = (0, 0);
            for c in d.iter_all_changes() {
                match c.tag() {
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Delete => removed += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            let rel = self.rel(&path);
            diff.push_str(
                &d.unified_diff()
                    .context_radius(2)
                    .header(&format!("a/{rel}"), &format!("b/{rel}"))
                    .to_string(),
            );
            changes.push(FileChange {
                path: rel,
                kind: if before.is_none() {
                    "created".into()
                } else {
                    "modified".into()
                },
                added,
                removed,
            });
        }
        (changes, diff)
    }
}

#[derive(Debug)]
pub struct Replaced {
    pub text: String,
    pub count: usize,
    pub fuzzy: bool,
}

/// Replaces `old` by `new` in `text`: exact first, then line-wise ignoring
/// indentation and trailing whitespace (unique match required).
pub fn replace(text: &str, old: &str, new: &str, all: bool) -> Result<Replaced, String> {
    let exact = text.matches(old).count();
    if exact == 1 || (exact > 1 && all) {
        return Ok(Replaced {
            text: if all {
                text.replace(old, new)
            } else {
                text.replacen(old, new, 1)
            },
            count: if all { exact } else { 1 },
            fuzzy: false,
        });
    }
    if exact > 1 {
        return Err(format!(
            "old_text occurs {exact} times – include more surrounding lines, or set replace_all"
        ));
    }
    // Fuzzy: compare trimmed lines.
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let old_lines: Vec<&str> = old.lines().filter(|l| !l.trim().is_empty()).collect();
    if old_lines.is_empty() {
        return Err("old_text contains only whitespace".into());
    }
    let norm = |s: &str| s.trim().to_string();
    let mut matches = Vec::new();
    'outer: for start in 0..lines.len() {
        let (mut i, mut j) = (start, 0);
        while j < old_lines.len() {
            if i >= lines.len() {
                continue 'outer;
            }
            if lines[i].trim().is_empty() {
                i += 1;
                continue;
            }
            if norm(lines[i]) != norm(old_lines[j]) {
                continue 'outer;
            }
            i += 1;
            j += 1;
        }
        matches.push((start, i));
    }
    match matches.as_slice() {
        [] => Err("old_text was not found. Read the file again and quote the text exactly (or use write_file for the whole file)".into()),
        [(start, end)] => {
            // Re-indent the new text like the matched text.
            let file_indent: String = lines[*start].chars().take_while(|c| c.is_whitespace() && *c != '\n').collect();
            let old_indent: String = old_lines[0].chars().take_while(|c| c.is_whitespace()).collect();
            let mut replacement = String::new();
            for l in new.lines() {
                let body = l.strip_prefix(old_indent.as_str()).unwrap_or(l);
                if body.is_empty() {
                    replacement.push('\n');
                } else {
                    replacement.push_str(&format!("{file_indent}{body}\n"));
                }
            }
            let had_newline = lines[end - 1].ends_with('\n');
            if !had_newline {
                replacement.pop();
            }
            let mut out: String = lines[..*start].concat();
            out.push_str(&replacement);
            out.push_str(&lines[*end..].concat());
            Ok(Replaced { text: out, count: 1, fuzzy: true })
        }
        many => Err(format!("old_text matches {} places (ignoring indentation) – include more context", many.len())),
    }
}

/// Stops a command's whole process group when dropped (finished, timed out
/// or cancelled) – background processes it started included.
struct ProcessGroup(Option<u32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(access: Access) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# Demo\nHello world\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/junk.rs"), "fn junk() {}\n").unwrap();
        let w = Workspace::new(dir.path(), access)
            .unwrap()
            .with_shell(ShellSettings {
                sandbox: false,
                ..Default::default()
            });
        (dir, w)
    }

    /// Background processes end with the command, a cancelled call ends the
    /// whole process group – nothing keeps running after the agent.
    #[tokio::test]
    async fn commands_leave_nothing_running() {
        let (d, w) = ws(Access::Shell);
        let started = std::time::Instant::now();
        let out = w
            .execute(
                "bash",
                &json!({"command": "(sleep 30; echo late > late.txt) & echo started"}),
            )
            .await;
        assert!(out.content.contains("started"), "{}", out.content);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "waited for the background job"
        );
        // A cancelled (dropped) call stops its children too.
        let args = json!({"command": "sh -c 'sleep 2; echo child > child.txt' & sleep 30"});
        let slow = w.execute("bash", &args);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), slow).await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !d.path().join("child.txt").exists(),
            "the child kept running"
        );
        assert!(!d.path().join("late.txt").exists());
    }

    #[tokio::test]
    async fn reads_with_line_numbers_and_ranges() {
        let (_d, w) = ws(Access::Read);
        let out = w
            .execute(
                "read_file",
                &json!({"path": "src/main.rs", "offset": 2, "limit": 1}),
            )
            .await;
        assert_eq!(
            out.content,
            "     2\t    let x = 1;\n… 2 more lines (read with offset=3)"
        );
        assert!(
            w.execute("read_file", &json!({"path": "nope.rs"}))
                .await
                .is_error
        );
    }

    // covers: M3-AC-06
    #[tokio::test]
    async fn permissions_and_workspace_boundaries_hold() {
        let (d, w) = ws(Access::Read);
        let out = w
            .execute("write_file", &json!({"path": "x.txt", "content": "x"}))
            .await;
        assert!(out.is_error && out.content.contains("not allowed"));
        assert!(w.execute("bash", &json!({"command": "ls"})).await.is_error);
        assert!(
            !w.definitions()
                .iter()
                .any(|t| t["function"]["name"] == "write_file")
        );
        let (_d2, w) = ws(Access::Edit);
        for bad in ["../outside.txt", "/etc/passwd", "src/../../x"] {
            let out = w
                .execute("write_file", &json!({"path": bad, "content": "x"}))
                .await;
            assert!(
                out.is_error && out.content.contains("outside the project"),
                "{bad}: {}",
                out.content
            );
        }
        // A symlink pointing outside does not help.
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), w.root().join("escape")).unwrap();
        let out = w
            .execute(
                "write_file",
                &json!({"path": "escape/pwned.txt", "content": "x"}),
            )
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(!outside.path().join("pwned.txt").exists());
        let _ = d;
    }

    // covers: M3-AC-03
    #[tokio::test]
    async fn concurrent_changes_are_detected() {
        let (_d, w) = ws(Access::Edit);
        w.execute("read_file", &json!({"path": "src/main.rs"}))
            .await;
        // Someone else changes the file after the agent read it.
        std::fs::write(
            w.root().join("src/main.rs"),
            "fn main() { /* changed */ }\n",
        )
        .unwrap();
        let out = w
            .execute(
                "edit_file",
                &json!({"path": "src/main.rs", "old_text": "let x = 1;", "new_text": "let x = 2;"}),
            )
            .await;
        assert!(
            out.is_error && out.content.contains("changed by someone else"),
            "{}",
            out.content
        );
        let out = w
            .execute(
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            )
            .await;
        assert!(out.is_error);
        // After reading again, the change works.
        w.execute("read_file", &json!({"path": "src/main.rs"}))
            .await;
        let out = w.execute("edit_file", &json!({"path": "src/main.rs", "old_text": "/* changed */", "new_text": "/* ok */"})).await;
        assert!(!out.is_error, "{}", out.content);
        // Overwriting an existing file requires having read it.
        let out = w
            .execute("write_file", &json!({"path": "README.md", "content": "x"}))
            .await;
        assert!(out.is_error && out.content.contains("read it first"));
    }

    // covers: M3-AC-02
    #[test]
    fn edits_tolerate_inexact_quotes_but_never_hit_the_wrong_place() {
        let text = "fn a() {\n    let x = 1;\n    if x > 0 {\n        run();\n    }\n}\n";
        // Exact.
        assert_eq!(
            replace(text, "let x = 1;", "let x = 2;", false)
                .unwrap()
                .text,
            text.replace("let x = 1;", "let x = 2;")
        );
        // Indentation differs in the quote → fuzzy, re-indented.
        let r = replace(
            text,
            "if x > 0 {\n    run();\n}",
            "if x > 1 {\n    go();\n}",
            false,
        )
        .unwrap();
        assert!(r.fuzzy);
        assert_eq!(
            r.text,
            "fn a() {\n    let x = 1;\n    if x > 1 {\n        go();\n    }\n}\n"
        );
        // Ambiguous → error, nothing changed.
        let dup = "x = 1\ny = 2\nx = 1\n";
        assert!(
            replace(dup, "x = 1", "x = 3", false)
                .unwrap_err()
                .contains("2 times")
        );
        assert_eq!(
            replace(dup, "x = 1", "x = 3", true).unwrap().text,
            "x = 3\ny = 2\nx = 3\n"
        );
        // Not found → instructive error.
        assert!(
            replace(text, "nonexistent", "y", false)
                .unwrap_err()
                .contains("not found")
        );
    }

    proptest::proptest! {
        // covers: M3-AC-02
        #[test]
        fn a_successful_edit_changes_exactly_one_matching_place(
            lines in proptest::collection::vec("[a-c ]{0,6}", 1..12),
            pick in 0usize..12,
        ) {
            let text = lines.join("\n") + "\n";
            let target = lines[pick % lines.len()].clone();
            if let Ok(r) = replace(&text, &target, "NEW", false) {
                // Nothing else changed: removing the replacement restores the rest.
                let changed = text.lines().zip(r.text.lines()).filter(|(a, b)| a != b).count();
                proptest::prop_assert!(changed <= target.lines().count().max(1));
            }
        }
    }

    #[tokio::test]
    async fn greps_and_globs_respect_gitignore() {
        let (_d, w) = ws(Access::Read);
        let out = w.execute("grep", &json!({"pattern": "fn \\w+"})).await;
        assert!(
            out.content.contains("src/main.rs:1: fn main() {"),
            "{}",
            out.content
        );
        assert!(
            !out.content.contains("junk"),
            "ignored files must not appear"
        );
        let out = w
            .execute(
                "grep",
                &json!({"pattern": "HELLO", "case_insensitive": true, "glob": "*.md"}),
            )
            .await;
        assert_eq!(out.content, "README.md:2: Hello world");
        let out = w.execute("glob", &json!({"pattern": "*.rs"})).await;
        assert_eq!(out.content, "src/main.rs");
        // An invalid regex falls back to a literal search.
        let out = w
            .execute("grep", &json!({"pattern": "println!(\"{x}"}))
            .await;
        assert!(out.content.contains("src/main.rs:3"), "{}", out.content);
    }

    #[tokio::test]
    async fn reports_changes_with_diff() {
        let (_d, w) = ws(Access::Edit);
        w.execute(
            "write_file",
            &json!({"path": "NOTES.md", "content": "# Notes\n"}),
        )
        .await;
        w.execute(
            "edit_file",
            &json!({"path": "README.md", "old_text": "Hello world", "new_text": "Hello Ancilo"}),
        )
        .await;
        let (changes, diff) = w.changes();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .any(|c| c.path == "NOTES.md" && c.kind == "created" && c.added == 1)
        );
        assert!(
            changes
                .iter()
                .any(|c| c.path == "README.md" && c.added == 1 && c.removed == 1)
        );
        assert!(diff.contains("+Hello Ancilo"));
    }

    #[test]
    fn paths_are_replaced_only_as_whole_paths() {
        let r = |s: &str| replace_path(s, "/a/wcs", "/w/s1");
        assert_eq!(r("/a/wcs"), "/w/s1");
        assert_eq!(r("cd /a/wcs && ls /a/wcs/src"), "cd /w/s1 && ls /w/s1/src");
        assert_eq!(r("'/a/wcs/x.txt'"), "'/w/s1/x.txt'");
        assert_eq!(r("in /a/wcs."), "in /w/s1.");
        assert_eq!(
            r("/a/wcs2 /x/a/wcs /a/wcs.git"),
            "/a/wcs2 /x/a/wcs /a/wcs.git"
        );
        assert_eq!(replace_path("same", "", "/x"), "same");
    }

    // covers: M8-AC-09
    /// A session's agent works in a copy of the project but only ever sees the
    /// project's own path: what it writes there lands in the copy, the
    /// project itself stays untouched.
    #[tokio::test]
    async fn the_agent_sees_the_project_path_and_works_in_the_copy() {
        let (copy, w) = ws(Access::Shell);
        let project = tempfile::tempdir().unwrap();
        let project_path = std::fs::canonicalize(project.path()).unwrap();
        let shown = project_path.display().to_string();
        let w = w.showing(project.path());
        let work = std::fs::canonicalize(copy.path())
            .unwrap()
            .display()
            .to_string();

        assert!(w.context().unwrap().contains(&shown));
        assert!(!w.context().unwrap().contains(&work));
        // Absolute project paths lead into the copy.
        let out = w
            .execute(
                "write_file",
                &json!({"path": format!("{shown}/hello.txt"), "content": "Hallo\n"}),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(copy.path().join("hello.txt").exists());
        assert!(!project.path().join("hello.txt").exists());
        let out = w
            .execute("read_file", &json!({"path": format!("{shown}/README.md")}))
            .await;
        assert!(out.content.contains("Hello world"), "{}", out.content);
        // Commands too – and their output names the project, not the copy.
        let out = w
            .execute(
                "bash",
                &json!({"command": format!("pwd; echo x > {shown}/b.txt; ls {shown}")}),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains(&shown), "{}", out.content);
        assert!(!out.content.contains(&work), "{}", out.content);
        assert!(out.content.contains("hello.txt"), "{}", out.content);
        assert!(copy.path().join("b.txt").exists());
        assert!(!project.path().join("b.txt").exists());
        // Outside stays outside, and the message names the project.
        let out = w.execute("read_file", &json!({"path": "/etc/hosts"})).await;
        assert!(
            out.is_error && out.content.contains(&shown),
            "{}",
            out.content
        );
    }
}
