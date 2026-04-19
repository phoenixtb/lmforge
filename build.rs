/// LMForge core build script.
///
/// On macOS: compiles `gpu_probe/main.swift` → `gpu_probe_bin/lmforge-gpu-probe-{triple}`
/// and emits `LMFORGE_GPU_PROBE_PATH` so that `server/sysinfo.rs` can embed the bytes
/// at compile time via `include_bytes!(env!("LMFORGE_GPU_PROBE_PATH"))`.
///
/// On other platforms: no-op (Linux/Windows use nvidia-smi / rocm-smi at runtime).
fn main() {
    #[cfg(target_os = "macos")]
    compile_gpu_probe();
}

#[cfg(target_os = "macos")]
fn compile_gpu_probe() {
    use std::process::Command;

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src = format!("{manifest}/gpu_probe/main.swift");
    let out_dir = format!("{manifest}/gpu_probe_bin");

    // Trigger rebuild only when the Swift source changes.
    println!("cargo:rerun-if-changed={src}");

    if !std::path::Path::new(&src).exists() {
        println!("cargo:warning=gpu_probe/main.swift not found — GPU probe unavailable");
        return;
    }

    // Target triple for the binary name (Tauri sidecar naming convention).
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let bin_name = format!("lmforge-gpu-probe-{arch}-apple-darwin");
    let out_path = format!("{out_dir}/{bin_name}");

    std::fs::create_dir_all(&out_dir).ok();

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
            // Expose the absolute path so sysinfo.rs can embed the bytes at compile time.
            println!("cargo:rustc-env=LMFORGE_GPU_PROBE_PATH={out_path}");
        }
        Ok(s) => {
            println!("cargo:warning=swiftc exited {s} — GPU probe will be unavailable");
        }
        Err(e) => {
            println!("cargo:warning=swiftc not found ({e}) — GPU probe will be unavailable");
        }
    }
}
