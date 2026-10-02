//! M9-AC-04: Homebrew installs Ancilo – the formula (CLI + daemon +
//! llama.cpp) and the cask (the app) – from a local tap, on a fresh runner.
//! It changes the machine's Homebrew, so it refuses to run elsewhere.
//!
//! Run (release CI): `ANCILO_BREW_TEST=1 ANCILO_ARCHIVE=… ANCILO_DMG=… cargo nextest run -p ancilo --test homebrew --run-ignored only`

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{cmd}: {e}"));
    assert!(
        out.status.success(),
        "{cmd} {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn sha256(path: &Path) -> String {
    run("shasum", &["-a", "256", path.to_str().unwrap()])
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn render(template: &str, version: &str, file: &Path) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/homebrew")
            .join(template),
    )
    .unwrap()
    .replace("@VERSION@", version)
    .replace("@URL@", &format!("file://{}", file.display()))
    .replace("@SHA256@", &sha256(file))
}

// covers: M9-AC-04
#[test]
#[ignore = "release check on a fresh runner: changes Homebrew (ANCILO_BREW_TEST=1)"]
fn homebrew_installs_the_formula_and_the_cask() {
    assert_eq!(
        std::env::var("ANCILO_BREW_TEST").as_deref(),
        Ok("1"),
        "only on a fresh CI runner – it installs into Homebrew"
    );
    let archive = PathBuf::from(std::env::var("ANCILO_ARCHIVE").expect("ANCILO_ARCHIVE"));
    let dmg = PathBuf::from(std::env::var("ANCILO_DMG").expect("ANCILO_DMG"));
    let version = env!("CARGO_PKG_VERSION");
    let tap = "local/ancilo-test";
    run("brew", &["tap-new", "--no-git", tap]);
    let repo = PathBuf::from(run("brew", &["--repository", tap]).trim());
    std::fs::create_dir_all(repo.join("Formula")).unwrap();
    std::fs::create_dir_all(repo.join("Casks")).unwrap();
    std::fs::write(
        repo.join("Formula/ancilo.rb"),
        render("ancilo.rb.in", version, &archive),
    )
    .unwrap();
    std::fs::write(
        repo.join("Casks/ancilo-app.rb"),
        render("ancilo-app.rb.in", version, &dmg),
    )
    .unwrap();

    // Formula: CLI, daemon and the shipped llama.cpp.
    run("brew", &["install", "--formula", &format!("{tap}/ancilo")]);
    assert!(run("ancilo", &["--version"]).contains(version));
    run("brew", &["test", &format!("{tap}/ancilo")]);
    let home = tempfile::tempdir().unwrap();
    let out = Command::new("ancilo")
        .args(["list", "--json"])
        .env("ANCILO_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Command::new("ancilo")
        .args(["daemon", "stop"])
        .env("ANCILO_HOME", home.path())
        .status()
        .ok();
    run("brew", &["uninstall", "--formula", "ancilo"]);

    // Cask: the app, with the `ancilo` command linked from inside it.
    run("brew", &["install", "--cask", &format!("{tap}/ancilo-app")]);
    assert!(Path::new("/Applications/Ancilo.app/Contents/MacOS/ancilo").is_file());
    assert!(Path::new("/Applications/Ancilo.app/Contents/MacOS/llama-server").is_file());
    assert!(run("ancilo", &["--version"]).contains(version));
    run("brew", &["uninstall", "--cask", "ancilo-app"]);
    run("brew", &["untap", tap]);
}
