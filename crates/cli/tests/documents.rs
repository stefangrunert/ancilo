//! Documents are read by the `ancilo` program in a process of its own –
//! sandboxed, without network, with a time limit (decision
//! `2026-10-03-drei-bereiche`): a broken file costs that process, never the
//! daemon, and the reader sees nothing of Ancilo's own data.

use std::path::PathBuf;

use ancilo_docs::{Extractor, Kind};

fn reader(scratch: &std::path::Path, hidden: Vec<PathBuf>) -> Extractor {
    Extractor::new(
        Some(PathBuf::from(env!("CARGO_BIN_EXE_ancilo"))),
        scratch.to_path_buf(),
        hidden,
    )
}

// covers: M10-AC-02
#[tokio::test]
async fn documents_are_read_in_a_process_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let ex = reader(&base.join("scratch"), vec![base.join("ancilo-home")]);
    let file = base.join("Notiz.md");
    std::fs::write(&file, "# Einkauf\nMilch, Brot").unwrap();
    let doc = ex.read(&file, &ex.workdir().unwrap()).await.unwrap();
    assert_eq!(doc.kind, Kind::Text);
    assert_eq!(doc.parts[0].text, "# Einkauf\nMilch, Brot");

    // A broken file: the reason comes back, Ancilo goes on.
    let broken = base.join("kaputt.pdf");
    std::fs::write(&broken, b"%PDF-1.7 not really").unwrap();
    let e = ex.read(&broken, &ex.workdir().unwrap()).await.unwrap_err();
    assert!(
        e.message().contains("cannot read the PDF"),
        "{}",
        e.message()
    );

    // Elsewhere on the disk is invisible to the reader: a document from
    // there is put into its place first (so this works), Ancilo's own data
    // never is (next test).

    // Its place is cleaned up after every reading.
    assert_eq!(std::fs::read_dir(base.join("scratch")).unwrap().count(), 0);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_reader_cannot_see_ancilos_own_data() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let home = base.join("ancilo-home");
    std::fs::create_dir_all(&home).unwrap();
    let secret = home.join("secrets.txt");
    std::fs::write(&secret, "key=s3cret").unwrap();
    let ex = reader(&home.join("tmp"), vec![home.clone()]);
    let e = ex.read(&secret, &ex.workdir().unwrap()).await.unwrap_err();
    assert!(!e.message().contains("s3cret"), "{}", e.message());
    // A file handed to it in its own place (an upload) is readable.
    let work = ex.workdir().unwrap();
    std::fs::write(work.join("upload.txt"), "hallo").unwrap();
    let doc = ex.read(&work.join("upload.txt"), &work).await.unwrap();
    assert_eq!(doc.parts[0].text, "hallo");
}

// covers: FPL-03 – a result is looked at like a document is read: in the
// sandboxed reader; its whole content reaches the checks, the app gets the
// first rows.
#[tokio::test]
async fn results_are_looked_at_in_the_reader_and_checked_whole() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let ex = reader(&base.join("scratch"), vec![base.join("ancilo-home")]);
    let mut rows: Vec<Vec<String>> = vec![vec!["Nr".into(), "Betrag".into()]];
    for i in 1..=300 {
        rows.push(vec![i.to_string(), "1".into()]);
    }
    rows.push(vec!["Summe".into(), "301".into()]);
    let file = base.join("Liste.xlsx");
    std::fs::write(
        &file,
        ancilo_docs::write::xlsx(&[ancilo_docs::write::Sheet {
            name: "Liste".into(),
            rows,
        }])
        .unwrap(),
    )
    .unwrap();
    let l = ex.layout(&file, &ex.workdir().unwrap()).await.unwrap();
    assert_eq!(l.sheets[0].rows.len(), 302, "all rows come back");
    let f = ancilo_docs::preview::check(&l, "");
    assert!(
        f.iter()
            .any(|x| x.place.as_deref() == Some("Liste!B302") && x.message.contains("300")),
        "{f:#?}"
    );
    assert_eq!(
        l.shown().sheets[0].rows.len(),
        ancilo_docs::preview::MAX_ROWS_SHOWN
    );
    // A broken file: said, nothing hangs.
    let broken = base.join("kaputt.xlsx");
    std::fs::write(&broken, b"PK not really").unwrap();
    let e = ex
        .layout(&broken, &ex.workdir().unwrap())
        .await
        .unwrap_err();
    assert!(e.message().contains("cannot read"), "{}", e.message());
    assert_eq!(std::fs::read_dir(base.join("scratch")).unwrap().count(), 0);
}
