// The app's web UI is embedded from `app/dist` (rust-embed). The folder must
// exist when compiling: debug builds then read the files at run time, so a
// fresh `npm run build` shows up without rebuilding Rust.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/dist");
    std::fs::create_dir_all(&dist).ok();
    println!("cargo:rerun-if-changed=build.rs");
}
