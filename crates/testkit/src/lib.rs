//! Deterministic stand-ins for every external dependency of Ancilo.
//!
//! - [`fake_llm`]: an OpenAI-compatible model server driven by scripts, also
//!   usable as a drop-in `llama-server` binary (`fake-llama-server`)
//! - [`fake_hf`]: a Hugging Face stand-in with fault injection
//! - [`fake_web`]: Wikipedia, Serper and web pages for the web search
//! - [`gguf`]: writes small but valid GGUF files
//! - [`home`]: isolated Ancilo homes, free ports, temporary git repositories

pub mod fake_hf;
pub mod fake_llm;
pub mod fake_web;
pub mod gguf;
pub mod home;

pub use fake_hf::{FakeFile, FakeHf, FakeRepo};
pub use fake_llm::{FakeLlm, Script};
pub use fake_web::FakeWeb;
pub use home::{TestHome, free_port};

/// Path of the `fake-llama-server` binary built alongside the tests.
///
/// Integration tests in other crates call this to point Ancilo's process
/// manager at the fake instead of real llama.cpp.
pub fn fake_llama_server_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_fake-llama-server") {
        return p.into();
    }
    // target/<profile>/deps/<test-binary> → target/<profile>/fake-llama-server
    let exe = std::env::current_exe().expect("current exe");
    let mut dir = exe.parent().expect("exe dir").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("fake-llama-server");
    assert!(
        candidate.exists(),
        "fake-llama-server not built at {} – run `cargo build -p ancilo-testkit --bins`",
        candidate.display()
    );
    candidate
}
