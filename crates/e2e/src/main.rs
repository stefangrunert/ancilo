//! `ancilo-e2e` – a real Ancilo daemon with fake Hugging Face, scripted fake
//! models and fake Claude Code/Codex, for the app's Playwright tests.
//!
//! ```text
//! ancilo-e2e [--ram-gib N] [--claude-fails] [--with-models] [--coding] [--web] [--openapi]
//! ```
//!
//! Prints one JSON line `{"url", "token", "home", "hf", "probe"}` (probe: the
//! computer's state the daemon sees – tests write it) and serves until standard
//! input closes (or SIGTERM). `--openapi` prints the OpenAPI document instead
//! and exits (used to generate the app's API types).

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ancilo_core::{Config, Paths};
use ancilo_daemon::DaemonOptions;
use ancilo_models::ManagerOptions;
use ancilo_models::download::DownloadOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, fake_llama_server_bin};
use serde_json::json;
use tokio::io::AsyncReadExt;

fn repo(id: &str, arch: &str, size: usize) -> FakeRepo {
    let name = id
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches("-GGUF")
        .to_string();
    FakeRepo::new(
        id,
        vec![
            FakeFile::gguf(&format!("{name}-Q8_0.gguf"), arch, 32768, size)
                .slow(Duration::from_millis(15)),
        ],
    )
    .with_gguf_meta(json!({"architecture": arch, "context_length": 32768}))
}

/// Ancilo's model list for the fake Hugging Face (newer than the bundled one).
const CATALOG: &str = r#"{"version": 1, "updated": "2099-01-01", "models": [
 {"id": "chat", "name": "Chat", "address": "hf.co/demo/Chat-GGUF", "maker": "Demo", "kind": "chat",
  "purposes": ["chat", "documents"], "params_b": 1.0, "active_params_b": 1.0,
  "quality": {"chat": 6, "documents": 6}, "sizes": {"Q8_0": 2000000}, "license": "mit", "released": "2099-01",
  "tested": true, "summary": {"de": "Ein guter Allrounder.", "en": "A good all-rounder."}},
 {"id": "writer", "name": "Writer", "address": "hf.co/demo/Writer-GGUF", "maker": "Demo", "kind": "chat",
  "purposes": ["chat"], "params_b": 1.0, "active_params_b": 1.0,
  "quality": {"chat": 5}, "sizes": {"Q8_0": 1000000}, "license": "mit", "released": "2099-01",
  "summary": {"de": "Schreibt gern.", "en": "Likes to write."}},
 {"id": "coder", "name": "Coder", "address": "hf.co/demo/Coder-GGUF", "maker": "Demo", "kind": "chat",
  "purposes": ["code"], "params_b": 1.0, "active_params_b": 1.0,
  "quality": {"code": 7}, "sizes": {"Q8_0": 1000000}, "license": "mit", "released": "2099-01",
  "summary": {"de": "Programmiert.", "en": "Codes."}},
 {"id": "tiny-embed", "name": "Tiny Embed", "address": "hf.co/demo/Tiny-Embed-GGUF", "maker": "Demo", "kind": "embedding",
  "purposes": ["documents"], "params_b": 0.02, "active_params_b": 0.02,
  "sizes": {"Q8_0": 200000}, "license": "mit", "released": "2099-01",
  "summary": {"de": "Macht Dokumente durchsuchbar.", "en": "Makes documents searchable."}}
]}"#;

/// The assistant: proposes to use Coder for delegated tasks, then explains.
const CHAT: &str = r#"
cycle: true
steps:
  - respond: { tool_calls: [{ name: assign_role, arguments: { role: delegation, model: coder-q8_0 } }] }
  - respond: { text: "I proposed to use **coder-q8_0** for delegated tasks – please confirm." }
"#;

/// The chat model with web search (`--web`): plans the lookup, then answers
/// from the sources.
const CHAT_WEB: &str = r#"
cycle: true
steps:
  - expect: { any_message_contains: "Classify the user's last message" }
    respond: { text: '{"type": "facts", "query": "Einwohnerzahl Oslo", "topic": "Oslo", "lang": "de"}' }
  - expect: { any_message_contains: "728.714" }
    respond: { text: "Oslo hat **728.714** Einwohner [1]." }
"#;

const CODER: &str = r##"
cycle: true
steps:
  - respond: { tool_calls: [{ name: write_file, arguments: { path: "NOTES.md", content: "# Notes\n" } }] }
  - respond: { text: "Created NOTES.md." }
"##;

const WRITER: &str = r#"
cycle: true
steps:
  - respond: { tool_calls: [{ name: write_file, arguments: { path: "NOTES.md", content: "notes\n" } }], delay_ms: 100 }
  - respond: { text: "Wrote NOTES.md.", delay_ms: 100 }
"#;

/// The coding agent (`--coding`): changes the README in place, then asks to
/// run a command (needs "shell"), then reports.
const DEV: &str = r##"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "README.md" } }] }
  - respond: { tool_calls: [{ name: edit_file, arguments: { path: "README.md", old_text: "# Demo", new_text: "# Demo\n\nHello from Dev." } }] }
  - respond: { tool_calls: [{ name: bash, arguments: { command: "echo ok > build.txt" } }] }
  - respond: { text: "Updated README.md and ran the build." }
fallback: { text: "Nothing else to do." }
"##;

/// The same task, another model – for "retry with another model".
const ALT: &str = r##"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "README.md" } }] }
  # The same start: the README as it was before the first attempt.
  - expect: { any_message_contains: "1\t# Demo" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "README.md", old_text: "# Demo", new_text: "# Demo (by Alt)" } }] }
  - respond: { text: "Alt renamed the heading." }
fallback: { text: "Nothing else to do." }
"##;

fn fake_cli(dir: &std::path::Path, name: &str, fails: bool) -> std::path::PathBuf {
    let script = if fails {
        "#!/bin/sh\necho \"error: not logged in – run \\`claude login\\`\" >&2\nexit 1\n"
            .to_string()
    } else {
        "#!/bin/sh\ncase \"$*\" in *\" list\"*) echo ancilo ;; esac\nexit 0\n".to_string()
    };
    let p = dir.join(name);
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let ram: u64 = value("--ram-gib")
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);

    let mut broken = FakeFile::gguf("Broken-Q8_0.gguf", "qwen3", 32768, 400_000);
    broken.fail_after_bytes = Some(50_000);
    broken.fail_times = u32::MAX;
    let hf = FakeHf::start_with_raw(
        vec![
            repo("demo/Chat-GGUF", "qwen3", 2_000_000),
            repo("demo/Coder-GGUF", "qwen3", 1_000_000),
            repo("demo/Writer-GGUF", "qwen3", 1_000_000),
            repo("demo/Tiny-Embed-GGUF", "bert", 200_000),
            repo("demo/Dev-GGUF", "qwen3", 300_000),
            repo("demo/Alt-GGUF", "qwen3", 300_000),
            FakeRepo::new("demo/Broken-GGUF", vec![broken])
                .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768})),
        ],
        vec![FakeFile::new(
            "ancilo/catalog.json",
            CATALOG.as_bytes().to_vec(),
        )],
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let scripts = dir.path().join("scripts");
    let bin = dir.path().join("bin");
    for d in [&home, &scripts, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    for (stem, s) in [
        ("Chat-Q8_0", if flag("--web") { CHAT_WEB } else { CHAT }),
        ("Coder-Q8_0", CODER),
        ("Writer-Q8_0", WRITER),
        ("Dev-Q8_0", DEV),
        ("Alt-Q8_0", ALT),
    ] {
        std::fs::write(scripts.join(format!("{stem}.yaml")), s).unwrap();
    }
    let hw = dir.path().join("hw.json");
    let profile = HardwareProfile {
        total_ram_bytes: ram << 30,
        gpu_memory_bytes: Some((ram << 30) * 3 / 4),
        ..HardwareProfile::apple(ram)
    };
    std::fs::write(&hw, serde_json::to_string(&profile).unwrap()).unwrap();
    // The computer's live state, as the app's cockpit shows it.
    let probe = dir.path().join("probe.json");
    let probe_path = probe.clone();
    std::fs::write(
        &probe,
        json!({"available_bytes": (ram << 30) / 2, "pressure": "normal", "thermal": "nominal", "swap_used_bytes": 0}).to_string(),
    )
    .unwrap();
    // The web for the web search: Wikipedia with one article.
    let web = ancilo_testkit::FakeWeb::start().await;
    web.article(
        "de",
        "Oslo",
        "Oslo ist die Hauptstadt Norwegens.\nDie Kommune Oslo hat 728.714 Einwohner (Stand: 1. Januar 2026).",
    );
    let paths = Paths::from_home(&home);
    let config = Config {
        port: 0,
        hf_endpoint: hf.url(),
        catalog_url: format!("{}/ancilo/catalog.json", hf.url()),
        llama_server_bin: Some(fake_llama_server_bin()),
        model_search_dirs: Some(vec![]),
        hardware_override: Some(hw),
        system_probe_override: Some(probe),
        wikipedia_endpoint: web.wikipedia(),
        serper_endpoint: web.serper(),
        llama_server_env: [
            (
                "FAKE_LLM_SCRIPT_DIR".to_string(),
                scripts.display().to_string(),
            ),
            ("FAKE_LLM_LOAD_MS".to_string(), "300".to_string()),
        ]
        .into_iter()
        .collect(),
        claude_bin: Some(fake_cli(&bin, "claude", flag("--claude-fails"))),
        codex_bin: Some(fake_cli(&bin, "codex", false)),
        codex_home: Some(dir.path().join("codex-home")),
        ancilo_bin: Some(fake_llama_server_bin().with_file_name("ancilo")),
        ..Config::default()
    };
    paths.ensure().unwrap();
    let options = DaemonOptions {
        manager: ManagerOptions {
            download: DownloadOptions {
                max_attempts: 3,
                base_backoff: Duration::from_millis(50),
                progress_interval: Duration::from_millis(50),
            },
            measure_speed: false,
            restart_backoff: Duration::from_millis(100),
            ..Default::default()
        },
        llama_build: Some(None),
        tasks: ancilo_tasks::Options {
            shell: ancilo_agent::ShellSettings {
                sandbox: false,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let d = ancilo_daemon::start(paths.clone(), config, options)
        .await
        .expect("daemon");
    let mut preload: Vec<&str> = Vec::new();
    if flag("--with-models") {
        preload.extend(["demo/Chat-GGUF", "demo/Tiny-Embed-GGUF"]);
    }
    if flag("--coding") {
        preload.extend(["demo/Dev-GGUF", "demo/Alt-GGUF"]);
    }
    {
        for address in preload {
            let v = d
                .manager
                .add(address, &Default::default(), false)
                .await
                .expect("add");
            for _ in 0..400 {
                if d.manager
                    .status(&v.id)
                    .await
                    .is_ok_and(|s| s.status == ancilo_models::manager::ModelStatus::Ready)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
    if flag("--openapi") {
        let doc: serde_json::Value = reqwest::Client::new()
            .get(format!("{}/api/v1/openapi.json", d.url()))
            .bearer_auth(&d.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
        d.stop().await;
        return;
    }
    println!(
        "{}",
        json!({"url": d.url(), "token": d.token, "home": home, "hf": hf.url(), "probe": probe_path})
    );
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 64];
    let mut term =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    tokio::select! {
        _ = async { while stdin.read(&mut buf).await.is_ok_and(|n| n > 0) {} } => {}
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = d.wait_for_shutdown_request() => {}
    }
    d.stop().await;
}
