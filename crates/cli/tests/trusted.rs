//! M9-AC-02: macOS trusts what users download – the app (DMG) is signed with
//! the Developer ID, notarized and stapled; the CLI runs under quarantine.
//!
//! Run (release job, after `packaging/sign.sh`):
//! `ANCILO_DMG=… ANCILO_ARCHIVE=… cargo nextest run -p ancilo --test trusted --run-ignored only`

#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(cmd).args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

// covers: M9-AC-02
#[test]
#[ignore = "release check: needs the signed, notarized DMG and archive"]
fn macos_trusts_the_downloads() {
    let dmg = std::env::var("ANCILO_DMG").expect("ANCILO_DMG");
    let archive = std::env::var("ANCILO_ARCHIVE").expect("ANCILO_ARCHIVE");
    // The DMG carries its notarization ticket.
    let (ok, text) = run("xcrun", &["stapler", "validate", &dmg]);
    assert!(ok, "{text}");
    let mnt = tempfile::tempdir().unwrap();
    let m = mnt.path().to_str().unwrap();
    let (ok, text) = run(
        "hdiutil",
        &["attach", "-nobrowse", "-readonly", "-mountpoint", m, &dmg],
    );
    assert!(ok, "{text}");
    let app = Path::new(m).join("Ancilo.app");
    let app = app.to_str().unwrap();
    let verify = run(
        "codesign",
        &["--verify", "--deep", "--strict", "--verbose=2", app],
    );
    let assess = run(
        "spctl",
        &["--assess", "--type", "execute", "--verbose", app],
    );
    run("hdiutil", &["detach", m]);
    assert!(verify.0, "{}", verify.1);
    assert!(
        assess.0 && assess.1.contains("Notarized Developer ID"),
        "{}",
        assess.1
    );
    // The CLI from the archive starts under quarantine, like a browser download.
    let dir = tempfile::tempdir().unwrap();
    let (ok, text) = run(
        "tar",
        &["xzf", &archive, "-C", dir.path().to_str().unwrap()],
    );
    assert!(ok, "{text}");
    let bin = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("bin/ancilo");
    let b = bin.to_str().unwrap();
    let (ok, text) = run("codesign", &["-dvv", b]);
    assert!(
        ok && text.contains("Authority=Developer ID Application"),
        "{text}"
    );
    run(
        "xattr",
        &["-w", "com.apple.quarantine", "0081;66000000;Safari;", b],
    );
    let (ok, text) = run(b, &["--version"]);
    assert!(ok && text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}
