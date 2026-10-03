//! The release app or the development app ("Ancilo Dev", built with the
//! `dev` feature). The development app lives next to the release without
//! touching it: its own name, data, port, login item, and no updates.

use std::path::PathBuf;

use ancilo_core::Paths;

pub const DEV: bool = cfg!(feature = "dev");
pub const NAME: &str = if DEV { "Ancilo Dev" } else { "Ancilo" };
/// The login item (LaunchAgent) that keeps the daemon running.
pub const LAUNCH_AGENT: &str = if DEV {
    "app.ancilo.dev.daemon"
} else {
    "app.ancilo.daemon"
};
const DEV_PORT: &str = "7425";

/// Where this app's daemon keeps its data.
pub fn paths() -> Paths {
    if DEV {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Paths::from_home(home.join("Library/Application Support/ancilo-dev"))
    } else {
        Paths::resolve()
    }
}

/// The environment the daemon is started with (empty for the release: its defaults).
pub fn daemon_env(paths: &Paths) -> Vec<(String, String)> {
    if DEV {
        vec![
            ("ANCILO_HOME".into(), paths.home().display().to_string()),
            ("ANCILO_PORT".into(), DEV_PORT.into()),
        ]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_keeps_its_defaults() {
        if !DEV {
            assert_eq!(NAME, "Ancilo");
            assert_eq!(LAUNCH_AGENT, "app.ancilo.daemon");
            assert!(daemon_env(&paths()).is_empty());
        }
    }
}
