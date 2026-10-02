//! M9-AC-05: operations named in the documentation exist – checked against
//! the registry of a real daemon.

use std::path::Path;

use ancilo_testkit::TestHome;
use serde_json::Value;

fn docs() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("README.md")];
    for e in std::fs::read_dir(root.join("docs")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "md") {
            files.push(p);
        }
    }
    files
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect()
}

fn snake(word: &str) -> bool {
    word.contains('_')
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
}

/// Operation names in the docs: after `ancilo op`, and every `snake_case`
/// word in backticks on a line that talks about operations.
fn referenced() -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (file, text) in docs() {
        for (i, line) in text.lines().enumerate() {
            let words: Vec<&str> = line.split_whitespace().collect();
            for w in words.windows(3) {
                if w[0].ends_with("ancilo") && w[1] == "op" {
                    let name: String = w[2]
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                        .collect();
                    if snake(&name) || !name.is_empty() && !name.contains('<') {
                        out.push((file.clone(), i + 1, name));
                    }
                }
            }
            if line.to_lowercase().contains("operation") {
                for (j, part) in line.split('`').enumerate() {
                    if j % 2 == 1 && snake(part) {
                        out.push((file.clone(), i + 1, part.to_string()));
                    }
                }
            }
        }
    }
    out
}

// covers: M9-AC-05
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operations_named_in_the_docs_exist() {
    let home = TestHome::new();
    let config = ancilo_core::Config {
        model_search_dirs: Some(vec![]),
        ..home.config()
    };
    let d = ancilo_daemon::start(
        home.paths.clone(),
        config,
        ancilo_daemon::DaemonOptions {
            llama_build: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ops: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/ops", d.url()))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    let refs = referenced();
    assert!(refs.len() > 15, "only {} references found", refs.len());
    let unknown: Vec<String> = refs
        .iter()
        .filter(|(_, _, n)| !names.contains(&n.as_str()))
        .map(|(f, l, n)| format!("{f}:{l} {n}"))
        .collect();
    d.stop().await;
    assert!(
        unknown.is_empty(),
        "operations in the docs that do not exist:\n{}",
        unknown.join("\n")
    );
}
