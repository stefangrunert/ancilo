//! A debug build (and its tests) on macOS gets the text recognition helper
//! too – built from `ocr/main.swift`, found through `ANCILO_OCR_BUILT`.
//! Release packages build and sign their own (packaging/package.sh). Without
//! the Swift compiler, documents are read as before – without recognition.

fn main() {
    println!("cargo:rerun-if-changed=ocr/main.swift");
    println!("cargo:rerun-if-changed=../../packaging/build-ocr.sh");
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    if !macos || !debug {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ancilo-ocr");
    let built = std::process::Command::new("../../packaging/build-ocr.sh")
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if built {
        println!("cargo:rustc-env=ANCILO_OCR_BUILT={}", out.display());
    } else {
        println!("cargo:warning=no text recognition in this build: swiftc failed or is missing");
    }
}
