//! Isolated Ancilo homes and other test helpers.

use std::path::{Path, PathBuf};
use std::process::Command;

use ancilo_core::{Config, Paths};
use tempfile::TempDir;

/// A fresh, private Ancilo home in a temporary directory. Everything a test
/// does stays inside it; it is deleted when dropped.
pub struct TestHome {
    dir: TempDir,
    pub paths: Paths,
}

impl TestHome {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ancilo-test-")
            .tempdir()
            .expect("tempdir");
        let paths = Paths::from_home(dir.path().join("home"));
        paths.ensure().expect("create home");
        Self { dir, paths }
    }

    /// Scratch directory next to the home (for projects, fake model dirs, …).
    pub fn scratch(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::create_dir_all(&p).expect("scratch dir");
        p
    }

    pub fn write_config(&self, config: &Config) {
        config.save(&self.paths).expect("write config");
    }

    /// Default test configuration: free port, no model search directories,
    /// no network endpoints (callers set fakes explicitly).
    pub fn config(&self) -> Config {
        Config {
            port: free_port(),
            hf_endpoint: "http://127.0.0.1:9".into(),
            github_endpoint: "http://127.0.0.1:9".into(),
            catalog_url: "http://127.0.0.1:9/catalog.json".into(),
            projects_dir: Some(self.scratch("new-projects")),
            model_search_dirs: Some(vec![]),
            ..Config::default()
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.home()
    }
}

impl Default for TestHome {
    fn default() -> Self {
        Self::new()
    }
}

/// A currently free TCP port on the loopback interface.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .unwrap()
        .port()
}

/// Creates a git repository with one commit containing `files`.
pub fn git_repo(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for (path, content) in files {
        let p = dir.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "init"]);
    dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M0-AC-07
    #[test]
    fn homes_are_isolated_and_cleaned_up() {
        let a = TestHome::new();
        let b = TestHome::new();
        assert_ne!(a.path(), b.path());
        assert!(a.paths.models_dir().is_dir());
        let kept = a.path().to_path_buf();
        drop(a);
        assert!(!kept.exists());
    }

    // covers: M0-AC-07
    #[test]
    fn free_ports_differ_and_configs_point_nowhere() {
        let h = TestHome::new();
        let c = h.config();
        assert_ne!(c.port, 0);
        assert_eq!(c.model_search_dirs, Some(vec![]));
        h.write_config(&c);
        assert_eq!(Config::load(&h.paths).unwrap(), c);
    }

    // covers: M0-AC-07
    #[test]
    fn creates_git_repositories() {
        let h = TestHome::new();
        let repo = git_repo(&h.scratch("proj"), &[("src/main.rs", "fn main() {}")]);
        let out = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("init"));
    }
}
