//! Links from the window to the web open in the user's browser – the window
//! shows only Ancilo.

use tauri::Url;

/// A web address outside Ancilo (`daemon`: Ancilo's own address).
pub fn outside(url: &Url, daemon: &str) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let ours = Url::parse(daemon).ok();
    ours.is_none_or(|o| o.origin() != url.origin())
}

/// Opens a web address in the default browser.
pub fn open(url: &Url) {
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    if let Err(e) = std::process::Command::new(program).arg(url.as_str()).spawn() {
        eprintln!("could not open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M7-AC-16
    #[test]
    fn web_links_leave_the_window_ancilos_own_stay() {
        let d = "http://127.0.0.1:7424";
        assert!(outside(&"https://serper.dev/".parse().unwrap(), d));
        assert!(outside(&"http://127.0.0.1:9999/x".parse().unwrap(), d), "another port is another site");
        assert!(!outside(&"http://127.0.0.1:7424/app/#/system".parse().unwrap(), d));
        assert!(!outside(&"about:blank".parse().unwrap(), d));
        assert!(!outside(&"file:///etc/passwd".parse().unwrap(), d));
    }
}
