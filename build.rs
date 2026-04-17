/// Root lmforge crate build script.
/// On macOS: emits LMFORGE_GPU_PROBE_PATH pointing to the pre-compiled Swift probe
/// so that server/sysinfo.rs can locate it via option_env!() without runtime path
/// searching in dev mode.  The probe must already be compiled by the tauri-ui
/// build.rs (which runs first thanks to Cargo's dependency ordering).
///
/// On other platforms: this file is a no-op (no Swift, no GPU probe).
fn main() {
    #[cfg(target_os = "macos")]
    emit_probe_path();
}

#[cfg(target_os = "macos")]
fn emit_probe_path() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // The probe lives in the adjacent ui/src-tauri/binaries/ directory.
    let probe = format!(
        "{manifest}/ui/src-tauri/binaries/lmforge-gpu-probe-aarch64-apple-darwin"
    );

    println!("cargo:rerun-if-changed={probe}");

    if std::path::Path::new(&probe).exists() {
        println!("cargo:rustc-env=LMFORGE_GPU_PROBE_PATH={probe}");
    }
}
