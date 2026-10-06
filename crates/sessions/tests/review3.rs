//! Keeping and saving a task's results take exactly what was seen – and a
//! failure on the way leaves a state that can be tried again (Codex review
//! round 3, findings 34 and 35).

use std::path::Path;

use ancilo_sessions::changes::Changes;
use ancilo_sessions::{Sessions, Terminals};

fn sessions(db: &ancilo_storage::Db, home: &Path) -> Sessions {
    let bus = ancilo_core::EventBus::in_memory();
    let config = ancilo_core::Config {
        model_search_dirs: Some(vec![]),
        llama_auto_install: Some(false),
        ..Default::default()
    };
    let manager = ancilo_models::ModelManager::new(
        ancilo_core::Paths::from_home(home),
        config,
        db.clone(),
        bus.clone(),
        ancilo_models::hardware::HardwareProfile::apple(8),
        None,
        Default::default(),
    );
    let gateway = ancilo_gateway::Gateway::new(manager, db.clone(), bus.clone());
    Sessions::new(
        db.clone(),
        bus.clone(),
        gateway,
        &ancilo_core::Paths::from_home(home),
        Default::default(),
        None,
        Terminals::new(bus),
    )
    .with_documents(std::sync::Arc::new(ancilo_docs::Extractor::new(
        None,
        home.join("scratch"),
        Vec::new(),
    )))
}

// covers: FPL-03 (keep exactly what was seen)
#[test]
fn keeping_applies_the_seen_version_or_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, copy_dir) = (tmp.path().join("source"), tmp.path().join("copy"));
    std::fs::create_dir_all(&source).unwrap();
    let mut changes = Changes::folder(&source, &copy_dir).unwrap();
    let Changes::Folder { copy, .. } = &changes else {
        unreachable!()
    };
    copy.write("a.txt", b"CHECKED A").unwrap();
    let seen = copy.version().unwrap();
    // Changed after the version was compared, before the plan is made.
    copy.write("a.txt", b"UNSEEN B").unwrap();
    assert!(changes.apply_seen(None, Some(&seen)).is_err());
    assert!(!source.join("a.txt").exists());
    // What is seen now is kept.
    let Changes::Folder { copy, .. } = &changes else {
        unreachable!()
    };
    let now = copy.version().unwrap();
    changes.apply_seen(None, Some(&now)).unwrap();
    assert_eq!(
        std::fs::read_to_string(source.join("a.txt")).unwrap(),
        "UNSEEN B"
    );
}

// covers: FPL-03 (saving results)
#[tokio::test]
async fn saving_that_cannot_be_noted_leaves_the_results_open_to_save_again() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = ancilo_storage::Db::in_memory().unwrap();
    db.with(|c| {
        c.execute_batch(
            "INSERT INTO models(id, name, source, state, added_at) VALUES('fake', 'fake', 'local', 'ready', '2026-10-06');
             INSERT INTO roles(role, model_id) VALUES('default', 'fake');",
        )
    })
    .unwrap();
    let s = sessions(&db, &home);
    let task = s.create_task(None, Some("Probe".into())).unwrap();
    std::fs::write(task.workdir.join("a.csv"), "Item,Amount\nA,10\nTotal,10").unwrap();
    let version = s.get(&task.id).unwrap().changes_version.unwrap();
    let dest = home.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    db.with(|c| {
        c.execute_batch(
            "CREATE TRIGGER fail_save BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL, 'injected'); END;",
        )
    })
    .unwrap();
    assert!(
        s.save_results(&task.id, Some(dest.clone()), Some(&version))
            .await
            .is_err()
    );
    db.with(|c| c.execute_batch("DROP TRIGGER fail_save;"))
        .unwrap();
    // Nothing saved, the results still open – and saved on the next try.
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    let got = s.get(&task.id).unwrap();
    assert_eq!(got.changes.len(), 1, "{:?}", got.changes);
    assert!(got.saved.is_none());
    let saved = s
        .save_results(&task.id, Some(dest.clone()), got.changes_version.as_deref())
        .await
        .unwrap();
    assert_eq!(saved.files.len(), 1);
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
}
