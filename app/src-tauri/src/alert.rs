//! What the app says when it cannot start: a plain macOS alert – there is no
//! window yet – in the Mac's language. Instead of quitting without a word.

/// Whether the Mac's first language is German.
pub fn german() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|langs| {
                langs
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                    .find(|l| !l.is_empty())
                    .map(|first| first.starts_with("de"))
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    std::env::var("LANG").is_ok_and(|l| l.starts_with("de"))
}

/// Ancilo runs from the disk image (or a copy macOS made of it): the login
/// item would point at a place that is gone after ejecting.
pub fn not_in_applications() -> (&'static str, String) {
    if german() {
        (
            "Bitte zuerst Ancilo nach „Programme“ ziehen",
            "Ancilo läuft gerade direkt aus dem Download. Zieh Ancilo im Fenster des Downloads auf den Ordner „Programme“ und öffne es dann dort.".into(),
        )
    } else {
        (
            "Please move Ancilo to Applications first",
            "Ancilo is running straight from the download. Drag Ancilo onto the Applications folder in the download's window, then open it from there.".into(),
        )
    }
}

/// The background service did not start.
pub fn no_daemon(why: &str, log: &std::path::Path) -> (&'static str, String) {
    if german() {
        (
            "Ancilo konnte nicht starten",
            format!(
                "Der Hintergrunddienst von Ancilo ließ sich nicht starten:\n\n{why}\n\nStarte den Mac neu und öffne Ancilo noch einmal. Hilft das nicht, schick uns die Datei {}.",
                log.display()
            ),
        )
    } else {
        (
            "Ancilo could not start",
            format!(
                "Ancilo's background service did not start:\n\n{why}\n\nRestart the Mac and open Ancilo again. If that does not help, send us the file {}.",
                log.display()
            ),
        )
    }
}

/// Shows the alert and waits for OK.
pub fn show((title, text): (&str, String)) {
    eprintln!("{title}: {text}");
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as critical buttons {\"OK\"} default button 1",
            "-e",
            "end run",
            title,
            &text,
        ])
        .status();
}
