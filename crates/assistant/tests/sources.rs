//! A source opened later says how its document stands – from the file
//! itself, never only from what Ancilo read of it last (Codex review round
//! 3, finding 36).

use std::path::Path;

use ancilo_assistant::Assistant;
use ancilo_assistant::conversations::{ChatKind, Conversation, ConversationMessage};
use ancilo_docs::evidence::{self, Now};
use ancilo_docs::library::Library;

fn gateway(
    db: &ancilo_storage::Db,
    home: &Path,
) -> (ancilo_gateway::Gateway, ancilo_core::EventBus) {
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
    (
        ancilo_gateway::Gateway::new(manager, db.clone(), bus.clone()),
        bus,
    )
}

/// Writes `text` into `file`, keeping its size and time.
fn change_quietly(file: &Path, text: &str) {
    let time = std::fs::metadata(file).unwrap().modified().unwrap();
    std::fs::write(file, text).unwrap();
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

// covers: FPL-01 (a source's file changed)
#[tokio::test]
async fn a_source_without_its_files_hash_never_says_same_from_an_old_reading() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let folder = tmp.path().join("docs");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("a.txt");
    std::fs::write(&file, "VERSION ONE").unwrap();
    let db = ancilo_storage::Db::in_memory().unwrap();
    let (gw, bus) = gateway(&db, &home);
    let ex = std::sync::Arc::new(ancilo_docs::Extractor::new(
        None,
        home.join("scratch"),
        Vec::new(),
    ));
    let lib = Library::new(db.clone(), ex, None);
    lib.refresh(&folder).await.unwrap();
    let mut ev = lib.evidence(&folder, "version").unwrap();
    evidence::number(&mut ev, 0);
    assert!(ev[0].file.is_some());
    // A source from before files were hashed.
    let mut old = ev[0].clone();
    old.id = "D2".into();
    old.file = None;
    let mut c = Conversation::new("Probe", ChatKind::Chat);
    let mut m = ConversationMessage::assistant("[D1] [D2]");
    m.evidence = vec![ev[0].clone(), old];
    c.messages.push(m);
    let a = Assistant::new(gw, bus, db.clone()).with_library(lib.clone());
    a.conversations().save(&c).unwrap();
    // Unchanged: both the same, as read.
    assert_eq!(a.open_evidence(&c.id, "D1").await.unwrap().now, Now::Same);
    assert_eq!(a.open_evidence(&c.id, "D2").await.unwrap().now, Now::Same);
    // Changed under the same size and time: the hashed one knows, the old
    // one does not claim "same".
    change_quietly(&file, "VERSION TWO");
    assert_eq!(
        a.open_evidence(&c.id, "D1").await.unwrap().now,
        Now::Changed
    );
    assert_eq!(
        a.open_evidence(&c.id, "D2").await.unwrap().now,
        Now::Unknown
    );
    // Read again: the old one compares against the reading of now.
    let now = evidence::file_hash(&file);
    for _ in 0..200 {
        if lib.hash_of(&folder, "a.txt") == now {
            break;
        }
        lib.refresh(&folder).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        a.open_evidence(&c.id, "D2").await.unwrap().now,
        Now::Changed
    );
}
