//! `lmforge doctor` — single-shot diagnostics for hardware + engine state.
//!
//! Designed to answer the "why is this slow / why does it crash on first
//! chat?" questions without booting the daemon. Read-only by design;
//! never mutates `~/.lmforge/`. Mirrors the same data the
//! `/lf/status` HTTP endpoint surfaces so the UI and the CLI can be
//! reconciled at a glance.

use anyhow::Result;

use crate::config::LmForgeConfig;
use crate::engine::variant::{self, LlamaVariant, VariantState};
use crate::hardware::{self, probe::HardwareProfile};

pub async fn run(config: &LmForgeConfig) -> Result<()> {
    let data_dir = config.data_dir();
    let profile = hardware::detect().unwrap_or_default();
    let variant_state = scan_variants(&data_dir, &profile);
    let active = variant::select(&profile, &variant_state);

    println!("lmforge doctor");
    println!("──────────────────────────────────────────────────────────────");
    print_hardware(&profile);
    println!();
    print_engine_state(&profile, &data_dir, active, &variant_state);
    println!();
    print_runtime_hints(&profile, active);
    Ok(())
}

fn print_hardware(profile: &HardwareProfile) {
    println!("Hardware");
    println!(
        "  os                : {:?} ({:?})",
        profile.os, profile.os_family
    );
    println!("  arch              : {:?}", profile.arch);
    println!(
        "  gpu               : {:?}{}",
        profile.gpu_vendor,
        if profile.gpu_count > 1 {
            format!(" x{}", profile.gpu_count)
        } else {
            String::new()
        },
    );
    match profile.compute_cap {
        Some((maj, min)) => println!("  compute_cap       : sm_{maj}{min}  ({maj}.{min})"),
        None => println!("  compute_cap       : (none)"),
    }
    println!("  vram_gb           : {:.1}", profile.vram_gb);
    println!(
        "  ram_gb            : {:.1}{}",
        profile.total_ram_gb,
        if profile.unified_mem {
            " (unified)"
        } else {
            ""
        }
    );
    println!(
        "  cpu               : {} ({} cores)",
        profile.cpu_model, profile.cpu_cores
    );

    if profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia {
        let runtime = profile
            .cuda_runtime_version
            .as_deref()
            .unwrap_or("(unknown)");
        let driver_str = profile
            .cuda_driver_version
            .as_deref()
            .unwrap_or("(unknown)");
        let driver_tuple = profile.driver_tuple;
        println!("  cuda_runtime      : {runtime}");
        println!("  cuda_driver       : {driver_str}");

        // Floors comparison — surfaces the most common UX failure mode
        // (Ubuntu 22.04 LTS ships r535 by default; users wonder why no CUDA).
        let drv = driver_tuple.unwrap_or((0, 0, 0));
        let cuda12_ok = drv >= variant::CUDA12_DRIVER_MIN;
        let cuda13_ok = drv >= variant::CUDA13_DRIVER_MIN;
        println!(
            "  cuda12_eligible   : {}  (floor: {}.{}.{:02})",
            yesno(cuda12_ok),
            variant::CUDA12_DRIVER_MIN.0,
            variant::CUDA12_DRIVER_MIN.1,
            variant::CUDA12_DRIVER_MIN.2,
        );
        println!(
            "  cuda13_eligible   : {}  (floor: {}.{}.{:02})",
            yesno(cuda13_ok),
            variant::CUDA13_DRIVER_MIN.0,
            variant::CUDA13_DRIVER_MIN.1,
            variant::CUDA13_DRIVER_MIN.2,
        );
    }
    println!(
        "  vulkan_loader     : {}",
        yesno(probe_vulkan_loader(profile.os))
    );
}

fn print_engine_state(
    profile: &HardwareProfile,
    data_dir: &std::path::Path,
    active: LlamaVariant,
    state: &VariantState,
) {
    println!("Engine — llama.cpp");
    println!(
        "  variants_dir      : {}/engines/llamacpp/variants/",
        data_dir.display()
    );

    for v in [
        LlamaVariant::Cuda12,
        LlamaVariant::Cuda13,
        LlamaVariant::Vulkan,
        LlamaVariant::Cpu,
    ] {
        let installed = matches!(
            (v, state),
            (
                LlamaVariant::Cuda12,
                VariantState {
                    cuda12_installed: true,
                    ..
                }
            ) | (
                LlamaVariant::Cuda13,
                VariantState {
                    cuda13_installed: true,
                    ..
                }
            ) | (
                LlamaVariant::Vulkan,
                VariantState {
                    vulkan_installed: true,
                    ..
                }
            ) | (
                LlamaVariant::Cpu,
                VariantState {
                    cpu_installed: true,
                    ..
                }
            )
        );
        let marker = if v == active { "  ◀ ACTIVE" } else { "" };
        let refuse = if !installed {
            match variant::refuse_reason(v, profile) {
                Ok(()) => {
                    "  (install with: lmforge engine install llamacpp --variant ".to_string()
                        + v.as_str()
                        + ")"
                }
                Err(reason) => format!("  ({reason})"),
            }
        } else {
            String::new()
        };
        let label = format!("variant {v}");
        println!(
            "  {label:<18}: {}{marker}{}",
            if installed { "installed" } else { "missing" },
            refuse
        );
    }

    // Legacy flat layout — `<data_dir>/engines/llama-server`. Still used by
    // existing installs until C-3 ports them to the variant tree.
    let legacy = data_dir.join("engines").join("llama-server");
    if legacy.is_file() {
        println!(
            "  legacy_binary     : {}  (pre-v0.2.0 layout)",
            legacy.display()
        );
    }

    if let Ok(manifest) = variant::Manifest::embedded() {
        println!("  manifest_tag      : {}", manifest.llamacpp_tag);
        println!(
            "  manifest_ready    : {}{}",
            yesno(manifest.is_ready()),
            if manifest.is_ready() {
                ""
            } else {
                "  (sha256 placeholders — CI dispatch pending)"
            }
        );
    }
}

fn print_runtime_hints(profile: &HardwareProfile, active: LlamaVariant) {
    println!("Runtime");
    let env_override = std::env::var("LMFORGE_LLAMACPP_VARIANT").ok();
    if let Some(v) = env_override.as_deref() {
        println!("  LMFORGE_LLAMACPP_VARIANT = {v}  (env override active)");
    } else {
        println!("  LMFORGE_LLAMACPP_VARIANT = (unset)");
    }
    println!("  active_variant    : {active}");

    // Hint specifically for the most common failure: Linux NVIDIA below
    // the cuda12 driver floor.
    if profile.os == crate::hardware::probe::Os::Linux
        && profile.gpu_vendor == crate::hardware::probe::GpuVendor::Nvidia
        && profile.driver_tuple.unwrap_or((0, 0, 0)) < variant::CUDA12_DRIVER_MIN
    {
        println!();
        println!(
            "  ⚠ NVIDIA driver is below the CUDA12 floor ({maj}.{min}.{patch:02}). \
             You will run on Vulkan, which is 15–25% slower than CUDA on consumer Blackwell.\n  \
             Fix: `sudo apt install nvidia-driver-570` (Ubuntu) or your distro's equivalent, \
             then re-run `lmforge engine install llamacpp --variant cuda12`.",
            maj = variant::CUDA12_DRIVER_MIN.0,
            min = variant::CUDA12_DRIVER_MIN.1,
            patch = variant::CUDA12_DRIVER_MIN.2,
        );
    }
}

fn scan_variants(data_dir: &std::path::Path, profile: &HardwareProfile) -> VariantState {
    crate::engine::installer::scan_variant_state(data_dir, profile)
}

fn yesno(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Mirror of `installer::vulkan_loader_available` so `doctor` doesn't
/// have to bring that internal helper into scope. Kept deliberately
/// minimal — full preflight lives in the installer.
fn probe_vulkan_loader(os: crate::hardware::probe::Os) -> bool {
    use crate::hardware::probe::Os;
    match os {
        Os::Linux => {
            for p in &[
                "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
                "/usr/lib64/libvulkan.so.1",
                "/usr/lib/libvulkan.so.1",
                "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
            ] {
                if std::path::Path::new(p).exists() {
                    return true;
                }
            }
            // ldconfig fallback — covers distros where libvulkan lives elsewhere.
            if let Ok(output) = std::process::Command::new("ldconfig").arg("-p").output()
                && let Ok(s) = String::from_utf8(output.stdout)
            {
                return s.contains("libvulkan.so.1");
            }
            false
        }
        Os::Windows => {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
            std::path::Path::new(&format!("{root}\\System32\\vulkan-1.dll")).exists()
        }
        _ => true,
    }
}
