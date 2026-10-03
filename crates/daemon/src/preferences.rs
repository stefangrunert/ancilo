//! How the app is used: the simple or the expert view, what Ancilo is for,
//! and how far the step-by-step setup has come. Kept in the daemon, so every
//! surface (and a reinstalled app) sees the same.

use std::collections::BTreeMap;

use ancilo_core::{Error, EventBus, NoInput, OpBuilder, Registry, Result};
use ancilo_models::catalog::Purpose;
use ancilo_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const KEY: &str = "preferences";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum View {
    /// For everyone: chat, build, set up – in plain words.
    #[default]
    Simple,
    /// Everything: models, roles, comparisons, diffs, terminal, details.
    Pro,
}

/// The setup steps, in order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// What Ancilo is for.
    Purpose,
    /// The AI that suits this computer.
    Model,
    /// How much of the computer Ancilo may take.
    Resources,
    /// Claude Code and Codex (optional).
    Connect,
    /// A first project, documents (optional).
    Projects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    Skipped,
    /// Not done yet (to redo a step).
    Open,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Preferences {
    #[serde(default)]
    pub view: View,
    /// What Ancilo is for (empty: not asked yet).
    #[serde(default)]
    pub purposes: Vec<Purpose>,
    /// Setup steps done or skipped.
    #[serde(default)]
    pub setup: BTreeMap<Step, StepState>,
    /// Folders whose documents chats may draw on (indexed for search).
    #[serde(default)]
    pub documents: Vec<std::path::PathBuf>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPreferences {
    #[serde(default)]
    pub view: Option<View>,
    #[serde(default)]
    pub purposes: Option<Vec<Purpose>>,
    /// A setup step and what became of it.
    #[serde(default)]
    pub step: Option<Step>,
    #[serde(default)]
    pub state: Option<StepState>,
    /// Let chats draw on the documents in this folder.
    #[serde(default)]
    pub add_documents: Option<std::path::PathBuf>,
    /// Stop drawing on this folder.
    #[serde(default)]
    pub remove_documents: Option<std::path::PathBuf>,
}

pub fn load(db: &Db) -> Preferences {
    db.get_setting(KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn apply(db: &Db, change: SetPreferences) -> Result<Preferences> {
    let mut p = load(db);
    if let Some(v) = change.view {
        p.view = v;
    }
    if let Some(purposes) = change.purposes {
        if purposes.is_empty() {
            return Err(Error::invalid("choose at least one purpose"));
        }
        p.purposes = purposes;
    }
    if let Some(dir) = change.add_documents {
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(Error::invalid(format!("not a folder: {}", dir.display())));
        }
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !p.documents.contains(&dir) {
            p.documents.push(dir);
        }
    }
    if let Some(dir) = change.remove_documents {
        let canonical = std::fs::canonicalize(&dir).unwrap_or(dir.clone());
        p.documents.retain(|d| *d != dir && *d != canonical);
    }
    match (change.step, change.state) {
        (Some(step), Some(StepState::Open)) => {
            p.setup.remove(&step);
        }
        (Some(step), Some(state)) => {
            p.setup.insert(step, state);
        }
        (None, None) => {}
        _ => return Err(Error::invalid("give both `step` and `state`")),
    }
    db.set_setting(KEY, &serde_json::to_string(&p)?)?;
    Ok(p)
}

/// `library`: reads the documents of chat projects (the `documents` folders).
pub fn register(
    registry: &mut Registry,
    db: Db,
    bus: EventBus,
    library: Option<ancilo_docs::library::Library>,
) {
    let d = db.clone();
    registry.register(
        OpBuilder::new("get_preferences")
            .summary(
                "How the app is used: simple or expert view, what Ancilo is for, setup progress",
            )
            .handler(move |_ctx, _i: NoInput| {
                let d = d.clone();
                async move { Ok(load(&d)) }
            }),
    );
    registry.register(
        OpBuilder::new("set_preferences")
            .summary("Switch between the simple and the expert view, set what Ancilo is for, or mark a setup step")
            .manage()
            .handler(move |_ctx, i: SetPreferences| {
                let db = db.clone();
                let bus = bus.clone();
                let library = library.clone();
                async move {
                    let (added, removed) = (i.add_documents.clone(), i.remove_documents.clone());
                    let p = apply(&db, i)?;
                    // A chat project's documents are read at once – and
                    // forgotten when it leaves the list.
                    if let Some(lib) = &library {
                        if let Some(dir) = added {
                            lib.refresh_soon(&dir);
                        }
                        if let Some(dir) = removed {
                            lib.forget(&dir)?;
                        }
                    }
                    bus.emit("preferences.changed", None, json!({"view": p.view}));
                    Ok(p)
                }
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M7-AC-14
    #[test]
    fn preferences_are_kept_and_steps_can_be_redone() {
        let db = Db::in_memory().unwrap();
        assert_eq!(load(&db), Preferences::default());
        assert_eq!(load(&db).view, View::Simple, "simple is the default");
        let p = apply(
            &db,
            SetPreferences {
                view: Some(View::Pro),
                purposes: Some(vec![Purpose::Chat, Purpose::Code]),
                step: Some(Step::Purpose),
                state: Some(StepState::Done),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(p.view, View::Pro);
        assert_eq!(load(&db).setup.get(&Step::Purpose), Some(&StepState::Done));
        let step = |step, state| SetPreferences {
            step: Some(step),
            state: Some(state),
            ..Default::default()
        };
        apply(&db, step(Step::Connect, StepState::Skipped)).unwrap();
        let p = apply(&db, step(Step::Purpose, StepState::Open)).unwrap();
        assert_eq!(p.setup.len(), 1);
        assert_eq!(p.setup.get(&Step::Connect), Some(&StepState::Skipped));
        let bad = SetPreferences {
            purposes: Some(vec![]),
            ..Default::default()
        };
        assert!(apply(&db, bad).is_err());
        let half = SetPreferences {
            step: Some(Step::Model),
            ..Default::default()
        };
        assert!(apply(&db, half).is_err());
        // Document folders: added once, removed again; only real folders.
        let dir = tempfile::tempdir().unwrap();
        let add = || SetPreferences {
            add_documents: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        apply(&db, add()).unwrap();
        assert_eq!(apply(&db, add()).unwrap().documents.len(), 1);
        let gone = SetPreferences {
            remove_documents: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        assert!(apply(&db, gone).unwrap().documents.is_empty());
        let missing = SetPreferences {
            add_documents: Some("/no/such/folder".into()),
            ..Default::default()
        };
        assert!(apply(&db, missing).is_err());
    }
}
