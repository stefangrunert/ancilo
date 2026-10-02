//! Splitting files into coherent, searchable units.
//!
//! - code (Rust, Python, JavaScript, TypeScript/TSX, Go, Java): tree-sitter –
//!   one unit per function, method, class or type, with its doc comment;
//!   code between definitions (imports, statements) is grouped into windows
//! - Markdown: one unit per section (heading)
//! - anything else: windows of 60 lines with 10 lines of overlap
//!
//! Units larger than [`MAX_LINES`] are split: containers (classes, impl
//! blocks) into their members, everything else into windows.

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser};

/// Larger units are split.
pub const MAX_LINES: usize = 150;
const WINDOW: usize = 60;
const OVERLAP: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// e.g. `Parser::parse`, `UserService.find`, a Markdown heading.
    pub symbol: Option<String>,
    /// function, method, class, type, section, code, text
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    Markdown,
    Text,
}

impl Lang {
    pub fn of(path: &str) -> Self {
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("rs") => Self::Rust,
            Some("py" | "pyi") => Self::Python,
            Some("js" | "jsx" | "mjs" | "cjs") => Self::JavaScript,
            Some("ts" | "mts" | "cts") => Self::TypeScript,
            Some("tsx") => Self::Tsx,
            Some("go") => Self::Go,
            Some("java") => Self::Java,
            Some("md" | "markdown" | "mdx") => Self::Markdown,
            _ => Self::Text,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::Java => "java",
            Self::Markdown => "markdown",
            Self::Text => "text",
        }
    }

    fn grammar(self) -> Option<Language> {
        Some(match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::Markdown | Self::Text => return None,
        })
    }

    /// Separator between container and member in symbol names.
    fn sep(self) -> &'static str {
        match self {
            Self::Rust => "::",
            _ => ".",
        }
    }
}

/// What a syntax node is for chunking.
enum Role {
    /// A unit of its own (kind name).
    Definition(&'static str),
    /// A unit that may be split into its members (kind name).
    Container(&'static str),
    /// Look inside (export statements, decorators, bodies).
    Transparent,
    Other,
}

fn role(lang: Lang, kind: &str) -> Role {
    use Role::*;
    match (lang, kind) {
        (Lang::Rust, "function_item" | "function_signature_item") => Definition("function"),
        (Lang::Rust, "struct_item" | "enum_item" | "union_item" | "type_item") => {
            Definition("type")
        }
        (Lang::Rust, "macro_definition") => Definition("macro"),
        (Lang::Rust, "const_item" | "static_item") => Definition("constant"),
        (Lang::Rust, "impl_item" | "trait_item") => Container("impl"),
        (Lang::Rust, "mod_item") => Container("module"),
        (Lang::Rust, "declaration_list") => Transparent,
        (Lang::Python, "function_definition") => Definition("function"),
        (Lang::Python, "class_definition") => Container("class"),
        (Lang::Python, "decorated_definition" | "block") => Transparent,
        (Lang::JavaScript | Lang::TypeScript | Lang::Tsx, k) => match k {
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                Definition("function")
            }
            "lexical_declaration" | "variable_declaration" => Definition("code"),
            "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                Definition("type")
            }
            "class_declaration" | "abstract_class_declaration" => Container("class"),
            "export_statement" | "class_body" => Transparent,
            _ => Other,
        },
        (Lang::Go, "function_declaration" | "method_declaration") => Definition("function"),
        (Lang::Go, "type_declaration") => Definition("type"),
        (Lang::Java, "method_declaration" | "constructor_declaration") => Definition("method"),
        (
            Lang::Java,
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration",
        ) => Container("class"),
        (Lang::Java, "class_body" | "interface_body" | "enum_body") => Transparent,
        _ => Other,
    }
}

fn node_name(lang: Lang, node: Node<'_>, src: &[u8]) -> Option<String> {
    let text = |n: Node<'_>| n.utf8_text(src).ok().map(str::to_string);
    if let Some(n) = node.child_by_field_name("name") {
        return text(n);
    }
    match (lang, node.kind()) {
        (Lang::Rust, "impl_item") => {
            let ty = node.child_by_field_name("type").and_then(text)?;
            match node.child_by_field_name("trait").and_then(text) {
                Some(tr) => Some(format!("{ty} ({tr})")),
                None => Some(ty),
            }
        }
        (Lang::Go, "type_declaration") => {
            let mut c = node.walk();
            let spec = node
                .named_children(&mut c)
                .find(|n| n.kind() == "type_spec")?;
            spec.child_by_field_name("name").and_then(text)
        }
        (_, "lexical_declaration" | "variable_declaration") => {
            let mut c = node.walk();
            let decl = node
                .named_children(&mut c)
                .find(|n| n.kind() == "variable_declarator")?;
            decl.child_by_field_name("name").and_then(text)
        }
        _ => None,
    }
}

struct Ctx<'a> {
    lang: Lang,
    src: &'a [u8],
    lines: Vec<&'a str>,
    out: Vec<Chunk>,
    /// Lines covered by a definition (0-based).
    covered: Vec<bool>,
}

impl Ctx<'_> {
    /// Extends a unit upwards over directly preceding comment/attribute lines.
    fn with_leading_comments(&self, start: usize) -> usize {
        let mut s = start;
        while s > 0 {
            let prev = self.lines[s - 1].trim_start();
            let is_comment = prev.starts_with("//")
                || prev.starts_with('#') && self.lang != Lang::Python
                || prev.starts_with("/*")
                || prev.starts_with('*')
                || prev.starts_with("@")
                    && matches!(
                        self.lang,
                        Lang::Java | Lang::Python | Lang::TypeScript | Lang::Tsx | Lang::JavaScript
                    )
                || prev.starts_with("#[")
                || self.lang == Lang::Python && prev.starts_with('#');
            if !is_comment || self.covered[s - 1] {
                break;
            }
            s -= 1;
        }
        s
    }

    fn emit(&mut self, start: usize, end: usize, symbol: Option<String>, kind: &str) {
        let end = end.min(self.lines.len().saturating_sub(1));
        if start > end {
            return;
        }
        for c in &mut self.covered[start..=end] {
            *c = true;
        }
        if end - start + 1 > MAX_LINES {
            for (s, e) in windows(start, end) {
                self.push(s, e, symbol.clone(), kind);
            }
        } else {
            self.push(start, end, symbol, kind);
        }
    }

    fn push(&mut self, start: usize, end: usize, symbol: Option<String>, kind: &str) {
        let text = self.lines[start..=end].join("\n");
        if text.trim().is_empty() {
            return;
        }
        self.out.push(Chunk {
            start_line: start + 1,
            end_line: end + 1,
            symbol,
            kind: kind.to_string(),
            text,
        });
    }

    fn walk(&mut self, node: Node<'_>, prefix: Option<&str>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            let qualified = |name: Option<String>| match (prefix, name) {
                (Some(p), Some(n)) => Some(format!("{p}{}{n}", self.lang.sep())),
                (None, n) => n,
                (Some(p), None) => Some(p.to_string()),
            };
            let (start, end) = (child.start_position().row, child.end_position().row);
            match role(self.lang, child.kind()) {
                Role::Definition(kind) => {
                    let name = qualified(node_name(self.lang, child, self.src));
                    let kind = if prefix.is_some() && kind == "function" {
                        "method"
                    } else {
                        kind
                    };
                    let s = self.with_leading_comments(start);
                    self.emit(s, end, name, kind);
                }
                Role::Container(kind) => {
                    let name = qualified(node_name(self.lang, child, self.src));
                    let s = self.with_leading_comments(start);
                    if end - s < MAX_LINES {
                        self.emit(s, end, name, kind);
                    } else {
                        // Members become units; the rest (signature, fields) too.
                        let body = child.child_by_field_name("body").unwrap_or(child);
                        self.walk(body, name.as_deref());
                        self.gaps(s, end, name, kind);
                    }
                }
                Role::Transparent => self.walk(child, prefix),
                Role::Other => {}
            }
        }
    }

    /// Uncovered, non-blank lines in `from..=to` become windows.
    fn gaps(&mut self, from: usize, to: usize, symbol: Option<String>, kind: &str) {
        let to = to.min(self.lines.len().saturating_sub(1));
        let mut i = from;
        while i <= to {
            if self.covered[i] || self.lines[i].trim().is_empty() {
                i += 1;
                continue;
            }
            let start = i;
            while i <= to && !self.covered[i] {
                i += 1;
            }
            let mut end = i - 1;
            while end > start && self.lines[end].trim().is_empty() {
                end -= 1;
            }
            for (s, e) in windows(start, end) {
                self.push(s, e, symbol.clone(), kind);
            }
            for c in &mut self.covered[start..=end] {
                *c = true;
            }
        }
    }
}

/// Windows of [`WINDOW`] lines with [`OVERLAP`] over `start..=end`.
fn windows(start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut s = start;
    loop {
        let e = (s + WINDOW - 1).min(end);
        out.push((s, e));
        if e >= end {
            break;
        }
        s = e + 1 - OVERLAP;
    }
    out
}

fn markdown(text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    let mut heads: Vec<(usize, String)> = Vec::new();
    let mut fence = false;
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("```") {
            fence = !fence;
        }
        if !fence && l.starts_with('#') {
            heads.push((i, l.trim_start_matches('#').trim().to_string()));
        }
    }
    let mut out = Vec::new();
    let mut starts: Vec<(usize, Option<String>)> = Vec::new();
    if heads.first().is_none_or(|h| h.0 > 0) {
        starts.push((0, None));
    }
    starts.extend(heads.into_iter().map(|(i, h)| (i, Some(h))));
    for (n, (start, title)) in starts.iter().enumerate() {
        let end = starts
            .get(n + 1)
            .map_or(lines.len(), |(s, _)| *s)
            .saturating_sub(1);
        if lines.is_empty() || *start > end {
            continue;
        }
        for (s, e) in windows(*start, end) {
            let body = lines[s..=e].join("\n");
            if body.trim().is_empty() {
                continue;
            }
            out.push(Chunk {
                start_line: s + 1,
                end_line: e + 1,
                symbol: title.clone(),
                kind: "section".into(),
                text: body,
            });
        }
    }
    out
}

fn plain(text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    windows(0, lines.len() - 1)
        .into_iter()
        .filter_map(|(s, e)| {
            let body = lines[s..=e].join("\n");
            (!body.trim().is_empty()).then(|| Chunk {
                start_line: s + 1,
                end_line: e + 1,
                symbol: None,
                kind: "text".into(),
                text: body,
            })
        })
        .collect()
}

/// Splits a file into units; ordered by position.
pub fn chunk_file(path: &str, text: &str) -> Vec<Chunk> {
    let lang = Lang::of(path);
    match lang {
        Lang::Markdown => return markdown(text),
        Lang::Text => return plain(text),
        _ => {}
    }
    let Some(grammar) = lang.grammar() else {
        return plain(text);
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return plain(text);
    }
    let Some(tree) = parser.parse(text, None) else {
        return plain(text);
    };
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let mut ctx = Ctx {
        lang,
        src: text.as_bytes(),
        lines,
        out: Vec::new(),
        covered: vec![false; n],
    };
    ctx.walk(tree.root_node(), None);
    if n > 0 {
        ctx.gaps(0, n - 1, None, "code");
    }
    let mut out = ctx.out;
    out.sort_by_key(|c| (c.start_line, c.end_line));
    out
}

/// Splits identifiers into words for full-text search:
/// `parseHttpRequest` → `parse http request`, `verify_token` → `verify token`.
pub fn split_identifier(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ':' || c == '.' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.extend(c.to_lowercase());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(chunks: &[Chunk]) -> String {
        chunks
            .iter()
            .map(|c| {
                format!(
                    "{}-{} {} {}",
                    c.start_line,
                    c.end_line,
                    c.kind,
                    c.symbol.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // covers: M5-AC-01
    #[test]
    fn rust_units_are_items_with_their_docs() {
        let src = r#"use std::io;

/// Checks the token.
pub fn verify_token(t: &str) -> bool {
    !t.is_empty()
}

pub struct Parser {
    pos: usize,
}

impl Parser {
    /// Parses.
    pub fn parse(&mut self) -> u32 {
        1
    }
}
"#;
        insta::assert_snapshot!(summary(&chunk_file("src/lib.rs", src)), @r"
        1-1 code -
        3-6 function verify_token
        8-10 type Parser
        12-17 impl Parser
        ");
    }

    // covers: M5-AC-01
    #[test]
    fn large_containers_are_split_into_members() {
        let mut src = String::from("class Service:\n    \"\"\"Handles users.\"\"\"\n\n");
        for i in 0..40 {
            src.push_str(&format!(
                "    def method_{i}(self, x):\n        y = x + {i}\n        return y\n\n"
            ));
        }
        let chunks = chunk_file("svc.py", &src);
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol.as_deref() == Some("Service.method_7") && c.kind == "method")
        );
        assert!(chunks.iter().any(|c| c.symbol.as_deref() == Some("Service") && c.text.contains("Handles users")));
        assert!(chunks.iter().all(|c| c.end_line - c.start_line < MAX_LINES));
    }

    // covers: M5-AC-01
    #[test]
    fn typescript_go_java_and_markdown() {
        let ts = "import { x } from './x';\n\nexport function handle(req: Req): Res {\n  return x(req);\n}\n\nexport interface Req { id: string }\n\nexport const makeId = () => 'id';\n";
        insta::assert_snapshot!(summary(&chunk_file("a.ts", ts)), @r"
        1-1 code -
        3-5 function handle
        7-7 type Req
        9-9 code makeId
        ");
        let go = "package main\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n\ntype Point struct {\n\tX int\n}\n\nfunc (p Point) Len() int { return p.X }\n";
        insta::assert_snapshot!(summary(&chunk_file("m.go", go)), @r"
        1-1 code -
        3-6 function Add
        8-10 type Point
        12-12 function Len
        ");
        let java = "package a;\n\npublic class Users {\n    public User find(String id) {\n        return null;\n    }\n}\n";
        insta::assert_snapshot!(summary(&chunk_file("Users.java", java)), @r"
        1-1 code -
        3-7 class Users
        ");
        let md = "# Title\n\nIntro.\n\n## Install\n\nRun it.\n\n```bash\n# not a heading\n```\n";
        insta::assert_snapshot!(summary(&chunk_file("README.md", md)), @r"
        1-4 section Title
        5-11 section Install
        ");
    }

    #[test]
    fn unknown_files_use_overlapping_windows() {
        let text: String = (1..=130).map(|i| format!("line {i}\n")).collect();
        let chunks = chunk_file("data.txt", &text);
        let spans: Vec<(usize, usize)> =
            chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
        assert_eq!(spans, vec![(1, 60), (51, 110), (101, 130)]);
    }

    #[test]
    fn identifiers_are_split_into_words() {
        assert_eq!(split_identifier("parseHttpRequest"), "parse http request");
        assert_eq!(split_identifier("verify_token"), "verify token");
        assert_eq!(split_identifier("Parser::parse"), "parser parse");
        assert_eq!(split_identifier("HTTPServer"), "httpserver");
    }
}
