fn main() {
    // Compile the Swift GPU probe on macOS only.
    // On other platforms this block is entirely omitted — no Swift toolchain required.
    #[cfg(target_os = "macos")]
    compile_gpu_probe();

    tauri_build::build()
}

/// Compile `gpu_probe/main.swift` → `binaries/lmforge-gpu-probe-{triple}`.
/// Emits `LMFORGE_GPU_PROBE_PATH` so `sysinfo.rs` can find the binary at
/// compile-time via `option_env!()` without runtime path searching in dev mode.
#[cfg(target_os = "macos")]
fn compile_gpu_probe() {
    use std::process::Command;

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src = format!("{manifest}/gpu_probe/main.swift");

    // Trigger a rebuild only when the Swift source changes.
    println!("cargo:rerun-if-changed={src}");

    if !std::path::Path::new(&src).exists() {
        // Source not present (e.g. vendored build without it). Skip silently.
        return;
    }

    // Determine the correct target triple for the binary name.
    // Tauri's externalBin convention: `name-{arch}-apple-darwin`.
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let bin_name = format!("lmforge-gpu-probe-{arch}-apple-darwin");
    let out_path = format!("{manifest}/binaries/{bin_name}");

    std::fs::create_dir_all(format!("{manifest}/binaries")).ok();

    let status = Command::new("swiftc")
        .args([
            "-O",
            "-o",
            &out_path,
            &src,
            "-framework",
            "IOKit",
            "-framework",
            "Foundation",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=lmforge-gpu-probe compiled → {out_path}");
            // Expose the path so sysinfo.rs can locate the binary at compile time.
            println!("cargo:rustc-env=LMFORGE_GPU_PROBE_PATH={out_path}");
        }
        Ok(s) => {
            println!("cargo:warning=swiftc exited {s} — GPU probe unavailable");
        }
        Err(e) => {
            println!("cargo:warning=swiftc not found ({e}) — GPU probe unavailable");
        }
    }
}
