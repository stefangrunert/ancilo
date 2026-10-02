use std::path::{Path, PathBuf};

/// Where Ancilo keeps its data.
///
/// The base directory can be overridden with `ANCILO_HOME` – every test runs in
/// its own fresh home so tests never touch the real installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    pub const ENV_HOME: &'static str = "ANCILO_HOME";

    /// `ANCILO_HOME` if set, otherwise the platform data directory
    /// (`~/Library/Application Support/ancilo` on macOS, XDG data dir on Linux).
    pub fn resolve() -> Self {
        if let Some(home) = std::env::var_os(Self::ENV_HOME).filter(|v| !v.is_empty()) {
            return Self::from_home(PathBuf::from(home));
        }
        let base = dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self::from_home(base.join("ancilo"))
    }

    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_file(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn token_file(&self) -> PathBuf {
        self.home.join("token")
    }

    /// Written by the running daemon: its address and PID.
    pub fn daemon_file(&self) -> PathBuf {
        self.home.join("daemon.json")
    }

    pub fn db_file(&self) -> PathBuf {
        self.home.join("ancilo.db")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.home.join("models")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.home.join("bin")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.home.join("worktrees")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.home.join("index")
    }

    /// Creates all directories.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.home.clone(),
            self.models_dir(),
            self.bin_dir(),
            self.logs_dir(),
            self.worktrees_dir(),
            self.index_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_below_home() {
        let p = Paths::from_home("/tmp/x");
        assert_eq!(p.db_file(), PathBuf::from("/tmp/x/ancilo.db"));
        assert_eq!(p.models_dir(), PathBuf::from("/tmp/x/models"));
    }

    #[test]
    fn ensure_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::from_home(dir.path().join("home"));
        p.ensure().unwrap();
        assert!(p.models_dir().is_dir());
        assert!(p.worktrees_dir().is_dir());
    }
}
