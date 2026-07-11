/// LMForge core build script.
///
/// On macOS: compiles `gpu_probe/main.swift` → `gpu_probe_bin/lmforge-gpu-probe-{triple}`
/// and emits `LMFORGE_GPU_PROBE_PATH` so that `server/sysinfo.rs` can embed the bytes
/// at compile time via `include_bytes!(env!("LMFORGE_GPU_PROBE_PATH"))`.
///
/// On other platforms: no-op (Linux/Windows use nvidia-smi / rocm-smi at runtime).
fn main() {
    emit_build_provenance();

    #[cfg(target_os = "macos")]
    compile_gpu_probe();

    #[cfg(target_os = "windows")]
    embed_windows_resources();
}

/// Embed VERSIONINFO, icon, and an asInvoker manifest into lmforge.exe.
/// A bare console exe with no publisher metadata is a classic Defender
/// heuristic trigger; this (plus Authenticode signing in the release
/// pipeline) is the standard false-positive mitigation.
#[cfg(target_os = "windows")]
fn embed_windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("ui/src-tauri/icons/icon.ico");
    res.set("ProductName", "LMForge");
    res.set(
        "FileDescription",
        "LMForge - local LLM inference orchestrator (daemon + CLI)",
    );
    res.set("CompanyName", "LMForge open source project");
    res.set(
        "LegalCopyright",
        "Copyright (c) LMForge contributors. MIT license.",
    );
    res.set("OriginalFilename", "lmforge.exe");
    res.set("InternalName", "lmforge");
    if let Err(e) = res.compile() {
        println!("cargo:warning=Windows resource embedding failed: {e}");
    }
}

/// Bake build provenance into the binary so `lmforge --version` self-identifies
/// the exact commit it was built from. The crate version (`0.1.5`) is static
/// across commits, so on its own it can't tell two builds apart — the embedded
/// git SHA + dirty flag + UTC build date close that gap. The bench reads this
/// from the *installed* binary, certifying the running daemon (not the checkout).
fn emit_build_provenance() {
    use std::process::Command;

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };

    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=LMFORGE_GIT_SHA={sha}");

    let date = utc_build_date();
    println!("cargo:rustc-env=LMFORGE_BUILD_DATE={date}");

    // Recompute provenance whenever HEAD moves or the working tree is touched.
    for p in [".git/HEAD", ".git/index"] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    // A new commit on the current branch changes the *ref* file, not .git/HEAD
    // itself (which just says "ref: refs/heads/<branch>"). Watch the resolved
    // ref so `git pull && cargo build` always re-embeds the right SHA.
    if let Ok(head) = std::fs::read_to_string(".git/HEAD")
        && let Some(reference) = head.strip_prefix("ref: ")
    {
        let ref_file = format!(".git/{}", reference.trim());
        if std::path::Path::new(&ref_file).exists() {
            println!("cargo:rerun-if-changed={ref_file}");
        }
    }
}

/// UTC `YYYY-MM-DD` from the system clock, no external crates
/// (civil-from-days, Howard Hinnant's algorithm).
fn utc_build_date() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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
