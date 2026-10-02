//! Drop-in replacement for `llama-server` in tests.
//!
//! Accepts llama-server's command line (`--model`, `--port`, `--host`,
//! `--alias`; everything else is ignored) and serves the fake-llm API.
//!
//! Environment:
//! - `FAKE_LLM_SCRIPT`: path to a YAML script (default: echo)
//! - `FAKE_LLM_SCRIPT_DIR`: per-model scripts `<dir>/<model file stem>.yaml`;
//!   takes precedence over `FAKE_LLM_SCRIPT` when the file exists
//! - `FAKE_LLM_LOAD_MS`: simulated model load time (`/health` answers 503)
//! - `FAKE_LLM_EXIT_AFTER_MS`: exit with code 1 after this long (crash test)
//! - `FAKE_LLM_CRASH_ONCE_MARKER`: with `FAKE_LLM_EXIT_AFTER_MS`, crash only if
//!   this file does not exist yet (it is created) – the restart then stays up
//! - `FAKE_LLM_ARGS_FILE`: write the received arguments to this file

use std::time::Duration;

use ancilo_testkit::fake_llm::{Options, Script, router, shared};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(file) = std::env::var("FAKE_LLM_ARGS_FILE") {
        std::fs::write(file, args.join("\n")).ok();
    }
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if args.iter().any(|a| a == "--version") {
        println!("version: 0 (fake)");
        return;
    }
    let model = value("--model").or_else(|| value("-m"));
    if let Some(m) = &model
        && !std::path::Path::new(m).exists()
    {
        eprintln!("error: failed to load model '{m}'");
        std::process::exit(1);
    }
    let host = value("--host").unwrap_or_else(|| "127.0.0.1".into());
    let port = value("--port").unwrap_or_else(|| "8080".into());
    let per_model = std::env::var("FAKE_LLM_SCRIPT_DIR").ok().and_then(|dir| {
        let stem = std::path::Path::new(model.as_deref()?)
            .file_stem()?
            .to_owned();
        let p = std::path::Path::new(&dir).join(format!("{}.yaml", stem.to_string_lossy()));
        p.exists().then_some(p)
    });
    let path = per_model.or_else(|| std::env::var("FAKE_LLM_SCRIPT").ok().map(Into::into));
    let mut script = match path {
        Some(path) => Script::from_yaml(&std::fs::read_to_string(path).expect("script"))
            .expect("valid script"),
        None => Script::default(),
    };
    if script.model.is_none() {
        script.model = value("--alias").or_else(|| {
            model.as_ref().and_then(|m| {
                std::path::Path::new(m)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
        });
    }
    let ms = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
    };
    let options = Options {
        load_time: ms("FAKE_LLM_LOAD_MS").unwrap_or_default(),
    };
    let crash_allowed = match std::env::var("FAKE_LLM_CRASH_ONCE_MARKER") {
        Ok(marker) if std::path::Path::new(&marker).exists() => false,
        Ok(marker) => {
            std::fs::write(marker, b"crashed").ok();
            true
        }
        Err(_) => true,
    };
    if let Some(after) = ms("FAKE_LLM_EXIT_AFTER_MS").filter(|_| crash_allowed) {
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            eprintln!("fake-llama-server: simulated crash");
            std::process::exit(1);
        });
    }
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
        .await
        .expect("bind");
    eprintln!("fake-llama-server listening on {host}:{port}");
    axum::serve(listener, router(shared(script, options)))
        .await
        .expect("serve");
}
