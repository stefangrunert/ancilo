//! Text recognition for pictures and scanned PDFs (macOS): the helper
//! `ancilo-ocr` (Swift, `crates/docs/ocr/main.swift`) asks the system's own
//! Vision framework – on this computer, without network. It runs in a
//! sandbox of its own, at low priority, with a time and a page limit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::{Error, Result};
use serde::Deserialize;

use crate::extract::{self, Extracted, Kind, Locator, Part, Warning};

/// How long recognizing one document may take.
pub const OCR_TIMEOUT: Duration = Duration::from_secs(180);
/// Pages of a scanned PDF that are recognized.
pub const MAX_PAGES: u32 = 50;
/// What the helper may print.
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// The helper next to the `ancilo` program – in the app bundle
/// (`Contents/MacOS/`) or the archive (`libexec/ancilo/`); in a debug build
/// also the one `build.rs` made. `None`: no text recognition here.
pub fn helper(exe: Option<&Path>) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        for p in [
            dir.join("ancilo-ocr"),
            dir.join("../libexec/ancilo/ancilo-ocr"),
        ] {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    #[cfg(debug_assertions)]
    if let Some(p) = option_env!("ANCILO_OCR_BUILT").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    None
}

/// Whether recognition could find text the reading did not: a picture, or a
/// PDF without any text (a scan).
pub fn wanted(doc: &Extracted) -> bool {
    doc.kind == Kind::Image || (doc.kind == Kind::Pdf && doc.warnings.contains(&Warning::NoText))
}

#[derive(Deserialize)]
struct Recognized {
    pages: Vec<String>,
}

/// The text of `file` (inside `workdir`), page by page.
pub async fn recognize(helper: &Path, file: &Path, workdir: &Path) -> Result<Vec<String>> {
    let mut cmd = command(helper, file, workdir)?;
    cmd.current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(Error::internal)?;
    let out = match tokio::time::timeout(OCR_TIMEOUT, child.wait_with_output()).await {
        Err(_) => return Err(Error::invalid("recognizing the text took too long")),
        Ok(r) => r.map_err(Error::internal)?,
    };
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("the text could not be recognized")
            .trim()
            .trim_start_matches("error: ")
            .to_string();
        return Err(Error::invalid(why));
    }
    if out.stdout.len() > MAX_OUTPUT {
        return Err(Error::invalid("this file holds too much text"));
    }
    let r: Recognized = serde_json::from_slice(&out.stdout).map_err(Error::internal)?;
    Ok(r.pages)
}

/// Puts what was recognized into `doc`: one part per page with text (a
/// single picture without a page), marked as recognized. Nothing found: the
/// document stays as it was ("no text").
pub fn merge(doc: &mut Extracted, pages: Vec<String>) {
    let single = doc.kind == Kind::Image && pages.len() == 1;
    let parts: Vec<Part> = pages
        .into_iter()
        .enumerate()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(i, text)| Part {
            at: (!single).then(|| Locator::Page(i as u32 + 1)),
            text,
        })
        .collect();
    if parts.is_empty() {
        if !doc.warnings.contains(&Warning::NoText) {
            doc.warnings.push(Warning::NoText);
        }
        return;
    }
    doc.parts = parts;
    doc.warnings.retain(|w| *w != Warning::NoText);
    doc.warnings.push(Warning::Recognized);
    extract::limit(doc);
}

/// The helper in a sandbox: like the document reader – its own place, the
/// system's libraries, no network, no other file of the user – plus what
/// Vision needs to work: system services (the graphics and neural engines,
/// its models), the folder of the helper itself and, inside the app, the app
/// bundle (Foundation reads the bundle the helper lives in – denied, Vision
/// fails with "Foundation._GenericObjCError error 0"). At low priority: the
/// computer stays usable.
#[cfg(target_os = "macos")]
fn command(helper: &Path, file: &Path, workdir: &Path) -> Result<tokio::process::Command> {
    let q = |p: &Path| p.display().to_string().replace(['"', '\\'], "");
    let real = std::fs::canonicalize(helper).unwrap_or_else(|_| helper.to_path_buf());
    let dir = |p: &Path| p.parent().map(q).unwrap_or_default();
    let bundles: String = [helper, real.as_path()]
        .iter()
        .filter_map(|p| {
            p.ancestors()
                .find(|a| a.extension().is_some_and(|e| e == "app"))
        })
        .map(|b| format!("(allow file-read* (subpath \"{}\"))\n", q(b)))
        .collect();
    let profile = format!(
        r#"(version 1)
(deny default)
(allow process-exec (literal "{bin}") (literal "{real}"))
(allow file-read* (literal "/") (literal "{bin}") (literal "{real}") (literal "{dir}") (literal "{real_dir}")
  (subpath "{work}") (subpath "/usr/lib") (subpath "/usr/share") (subpath "/System")
  (subpath "/private/var/db/dyld") (subpath "/Library/Apple") (literal "/dev/null")
  (literal "/dev/urandom") (literal "/dev/random") (literal "/private/etc/localtime"))
(allow file-read-metadata)
(allow file-write* (subpath "{work}") (literal "/dev/null"))
(allow sysctl-read)
(allow mach-lookup)
(allow iokit-open)
{bundles}"#,
        bin = q(helper),
        real = q(&real),
        dir = dir(helper),
        real_dir = dir(&real),
        work = q(workdir),
    );
    let mut c = tokio::process::Command::new("/usr/bin/nice");
    c.args(["-n", "10", "/usr/bin/sandbox-exec", "-p"])
        .arg(profile)
        .arg(helper)
        .arg(file)
        .arg(MAX_PAGES.to_string())
        .env_clear()
        .env("LANG", "C.UTF-8");
    Ok(c)
}

#[cfg(not(target_os = "macos"))]
fn command(_helper: &Path, _file: &Path, _workdir: &Path) -> Result<tokio::process::Command> {
    Err(Error::unavailable("text recognition needs macOS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(kind: Kind, warnings: Vec<Warning>) -> Extracted {
        Extracted {
            kind,
            parts: Vec::new(),
            warnings,
        }
    }

    #[test]
    fn recognized_text_becomes_pages_and_is_marked() {
        let mut scan = doc(Kind::Pdf, vec![Warning::NoText]);
        assert!(wanted(&scan));
        merge(
            &mut scan,
            vec!["Seite eins".into(), "  ".into(), "Seite drei".into()],
        );
        assert_eq!(
            scan.parts.iter().map(|p| p.at.clone()).collect::<Vec<_>>(),
            [Some(Locator::Page(1)), Some(Locator::Page(3))]
        );
        assert_eq!(scan.warnings, [Warning::Recognized]);
        let mut photo = doc(Kind::Image, vec![Warning::NoText]);
        merge(&mut photo, vec!["Quittung".into()]);
        assert_eq!(photo.parts[0].at, None, "one picture, no page");
        let mut blank = doc(Kind::Image, vec![Warning::NoText]);
        merge(&mut blank, vec![String::new()]);
        assert!(blank.parts.is_empty() && blank.warnings == [Warning::NoText]);
        // A PDF with text is not recognized again.
        assert!(!wanted(&doc(Kind::Pdf, Vec::new())));
    }
}
