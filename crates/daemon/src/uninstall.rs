//! Removing Ancilo from this computer: `ancilo uninstall`, and in the app
//! System › Remove Ancilo (which then also removes the login item and the
//! app itself).
//!
//! What Ancilo set up elsewhere is undone first – Claude Code and Codex no
//! longer start an `ancilo` that is gone. Unless the data is kept: the keys
//! in the keychain and the data directory (models Ancilo downloaded,
//! conversations, settings, logs). Never touched: what is the user's own –
//! files saved to Documents, the projects folder, models of LM Studio, Ollama
//! or the Hugging Face cache Ancilo only used.
//!
//! The daemon must be stopped before (the CLI does that).

use std::path::{Path, PathBuf};

use ancilo_core::{Config, Error, Paths, Result};
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Removed {
    /// Clients that no longer use Ancilo (`claude_code`, `codex`).
    pub disconnected: Vec<String>,
    /// Keys deleted from the keychain.
    pub keys: usize,
    /// The data directory, if it was deleted.
    pub data: Option<PathBuf>,
    /// What could not be undone – said, not hidden; the rest went on.
    pub problems: Vec<String>,
}

/// Removes this home's traces. `keep_data`: the data directory and the keys
/// stay (for installing again later).
pub fn uninstall(paths: &Paths, keep_data: bool) -> Result<Removed> {
    let config = Config::load(paths).unwrap_or_default();
    uninstall_with(paths, &config, keep_data, |service, models| {
        crate::secrets::delete_all(service, models)
    })
}

/// [`uninstall`] with the keychain step given (tests: no real keychain).
pub fn uninstall_with(
    paths: &Paths,
    config: &Config,
    keep_data: bool,
    delete_keys: impl FnOnce(&str, &[String]) -> Result<usize>,
) -> Result<Removed> {
    let home = paths.home();
    if !keep_data {
        check_home(home)?;
    }
    let mut removed = Removed::default();
    for (client, outcome) in crate::clients(paths, config).disconnect_all() {
        match outcome {
            Ok(_) => removed.disconnected.push(client),
            Err(e) => removed.problems.push(ancilo_core::messages::msg(
                "remove.not_disconnected",
                &[
                    (
                        "client",
                        &if client == "codex" {
                            "Codex"
                        } else {
                            "Claude Code"
                        },
                    ),
                    ("why", &e),
                ],
            )),
        }
    }
    if keep_data {
        return Ok(removed);
    }
    // The service name depends on the home – computed while it exists.
    let service = paths.keychain_service();
    match delete_keys(&service, &model_keys(paths)) {
        Ok(n) => removed.keys = n,
        Err(e) => removed.problems.push(e.to_string()),
    }
    std::fs::remove_dir_all(home)
        .map_err(|e| Error::internal(format!("cannot delete {}: {e}", home.display())))?;
    removed.data = Some(home.to_path_buf());
    Ok(removed)
}

/// Deletes a directory only if it is an Ancilo home – never the user's home
/// folder or one above it, never a folder without Ancilo's own files (a
/// mistyped `ANCILO_HOME`).
fn check_home(home: &Path) -> Result<()> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let home = canon(home);
    let user = dirs::home_dir().map(|h| canon(&h));
    let too_wide = home.parent().is_none() || user.is_some_and(|u| u.starts_with(&home));
    let ancilo = ["ancilo.db", "token", "config.toml"]
        .iter()
        .any(|f| home.join(f).is_file());
    if too_wide || !ancilo {
        return Err(Error::invalid(format!(
            "{} does not look like Ancilo's data directory – it is left as it is",
            home.display()
        )));
    }
    Ok(())
}

/// The key names of the home's models (`model:<id>`) – for keychains that
/// cannot be listed.
fn model_keys(paths: &Paths) -> Vec<String> {
    if !paths.db_file().is_file() {
        return Vec::new();
    }
    ancilo_storage::Db::open(&paths.db_file())
        .and_then(|db| {
            db.with(|c| {
                let mut s = c.prepare("SELECT id FROM models")?;
                let ids = s.query_map([], |r| r.get::<_, String>(0))?;
                ids.collect::<ancilo_storage::rusqlite::Result<Vec<String>>>()
            })
        })
        .map(|ids| {
            ids.iter()
                .map(|id| ancilo_models::manager::secret_name(id))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(dir.path().join("ancilo"));
        std::fs::create_dir_all(paths.home().join("models/a")).unwrap();
        std::fs::write(paths.home().join("token"), "t").unwrap();
        std::fs::write(paths.home().join("models/a/a.gguf"), "gguf").unwrap();
        (dir, paths)
    }

    #[test]
    fn removing_deletes_the_home_and_its_keys() {
        let (_dir, paths) = home();
        let mut asked = None;
        let r = uninstall_with(&paths, &Config::default(), false, |service, _| {
            asked = Some(service.to_string());
            Ok(2)
        })
        .unwrap();
        assert!(!paths.home().exists());
        assert_eq!(r.keys, 2);
        assert_eq!(r.data.as_deref(), Some(paths.home()));
        assert!(r.problems.is_empty());
        let service = asked.unwrap();
        assert!(
            service.starts_with("ancilo:"),
            "this home's keys only: {service}"
        );
    }

    #[test]
    fn keeping_the_data_keeps_home_and_keys() {
        let (_dir, paths) = home();
        let r = uninstall_with(&paths, &Config::default(), true, |_, _| {
            panic!("keys are kept")
        })
        .unwrap();
        assert!(paths.home().join("models/a/a.gguf").exists());
        assert!(r.data.is_none());
    }

    #[test]
    fn a_folder_that_is_no_ancilo_home_is_never_deleted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("thesis.docx"), "mine").unwrap();
        let paths = Paths::from_home(dir.path());
        let e = uninstall_with(&paths, &Config::default(), false, |_, _| Ok(0)).unwrap_err();
        assert!(e.to_string().contains("does not look like"), "{e}");
        assert!(dir.path().join("thesis.docx").exists());
        // The user's home folder – even with an Ancilo file in it.
        assert!(check_home(&dirs::home_dir().unwrap()).is_err());
    }

    #[test]
    fn a_keychain_that_refuses_is_said_and_the_rest_goes_on() {
        let (_dir, paths) = home();
        let r = uninstall_with(&paths, &Config::default(), false, |_, _| {
            Err(Error::unavailable("keychain locked"))
        })
        .unwrap();
        assert!(!paths.home().exists());
        assert_eq!(r.problems.len(), 1);
    }
}
