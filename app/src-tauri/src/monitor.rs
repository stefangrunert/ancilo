//! A macOS notification when the computer is at its limit and Ancilo's window
//! is not in front (decision `2026-10-02-systemmonitor`). The window shows
//! everything else itself; this only reaches the user elsewhere – rarely.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::daemon::{self, Daemon};

/// How often the app looks.
pub const EVERY: Duration = Duration::from_secs(15);
/// At most one notification in this time.
pub const QUIET: Duration = Duration::from_secs(10 * 60);

/// What the watcher remembers between looks.
#[derive(Debug, Default)]
pub struct Watch {
    critical: bool,
    last: Option<Instant>,
}

impl Watch {
    /// Whether to notify now: only on becoming critical, only when the window
    /// is not in front, and not again within [`QUIET`].
    pub fn decide(&mut self, critical: bool, in_front: bool, now: Instant) -> bool {
        let entered = critical && !self.critical;
        self.critical = critical;
        let quiet = self.last.is_some_and(|t| now.duration_since(t) < QUIET);
        if entered && !in_front && !quiet {
            self.last = Some(now);
            true
        } else {
            false
        }
    }
}

/// Title and text in the user's language, from the verdict's causes.
pub fn text(german: bool, causes: &[String]) -> (String, String) {
    let cause = causes.first().map(String::as_str).unwrap_or("memory");
    if german {
        let what = match cause {
            "heat" => "Der Rechner ist sehr heiß.",
            "cpu" => "Der Prozessor ist voll ausgelastet.",
            _ => "Der Arbeitsspeicher ist fast voll.",
        };
        (
            "Dein Rechner ist am Limit".into(),
            format!("{what} Öffne Ancilo – dort hilft ein Klick."),
        )
    } else {
        let what = match cause {
            "heat" => "The computer is very hot.",
            "cpu" => "The processor is fully loaded.",
            _ => "The memory is almost full.",
        };
        (
            "Your computer is at its limit".into(),
            format!("{what} Open Ancilo – one click helps there."),
        )
    }
}

/// Whether macOS is set to German (the app's own choice lives in the window).
fn german() -> bool {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = s.split('"').nth(1) {
            return first.starts_with("de");
        }
    }
    std::env::var("LANG").is_ok_and(|l| l.starts_with("de"))
}

/// Looks at the daemon's verdict every [`EVERY`] until the app ends.
pub fn spawn(app: AppHandle, d: Daemon, window: &'static str) {
    std::thread::spawn(move || {
        let german = german();
        let mut watch = Watch::default();
        loop {
            std::thread::sleep(EVERY);
            let Some(h) = daemon::op(&d, "system_health") else {
                continue;
            };
            let in_front = app.get_webview_window(window).is_some_and(|w| {
                w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)
            });
            if watch.decide(h["level"] == "critical", in_front, Instant::now()) {
                let causes: Vec<String> = h["causes"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let (title, body) = text(german, &causes);
                let _ = app.notification().builder().title(title).body(body).show();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M7-AC-15
    #[test]
    fn notifies_only_on_becoming_critical_out_of_sight_and_rarely() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert!(!w.decide(false, false, t0), "calm");
        assert!(
            !w.decide(true, true, t0),
            "the window is in front: it shows there"
        );
        assert!(!w.decide(true, false, t0), "still the same emergency");
        assert!(!w.decide(false, false, t0));
        assert!(
            w.decide(true, false, t0 + Duration::from_secs(1)),
            "critical again, window away"
        );
        assert!(!w.decide(false, false, t0 + Duration::from_secs(60)));
        assert!(
            !w.decide(true, false, t0 + Duration::from_secs(120)),
            "not again within ten minutes"
        );
        assert!(!w.decide(false, false, t0 + QUIET));
        assert!(w.decide(true, false, t0 + QUIET + Duration::from_secs(5)));
    }

    #[test]
    fn says_what_is_wrong_in_the_users_language() {
        let (title, body) = text(true, &["heat".into()]);
        assert_eq!(title, "Dein Rechner ist am Limit");
        assert!(body.starts_with("Der Rechner ist sehr heiß."));
        let (title, body) = text(false, &[]);
        assert_eq!(title, "Your computer is at its limit");
        assert!(body.starts_with("The memory is almost full."));
    }
}
