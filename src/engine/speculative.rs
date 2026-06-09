//! Speculative-decoding resolution for `llama-server`.
//!
//! Phase S-2 of the v0.2.0 plan. This module owns the decision "given
//! the model's MTP capability, the user's `[speculative]` config, and
//! the live VRAM budget — what spec-dec flags should we pass to
//! `llama-server`?".
//!
//! The output is intentionally a *plan* (a `SpecResolved`) instead of a
//! pre-formatted argv list — the adapter owns final flag formatting so
//! we can change one without touching the other.
//!
//! Pure: no env reads, no I/O, no panics. Tested via the unit matrix at
//! the bottom of this file.

use serde::{Deserialize, Serialize};

use crate::hardware::probe::GpuVendor;

// ── Public types ─────────────────────────────────────────────────────────────

/// The four modes a user can select via the `[speculative].mode` config
/// knob, the `LMFORGE_SPECULATIVE_MODE` env override, or
/// `lmforge start --speculative <mode>` (planned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SpecMode {
    /// "Pick the best path for the model + hardware automatically":
    ///   * `caps.mtp == Some(true)` and budget allows → MTP.
    ///   * else → Off (draft-model pairing is opt-in only; never auto).
    #[default]
    Auto,
    /// Force MTP on. If `caps.mtp != Some(true)`, the adapter is free to
    /// drop back to Off and log a warning at spawn time.
    Mtp,
    /// Force the draft-model pairing. Requires `[speculative].draft_model`
    /// to be set (today) or a curated pair to exist in
    /// `data/draft_pairs.toml` (future, S-2 follow-up).
    #[serde(rename = "draft-model")]
    DraftModel,
    /// Disable speculative decoding entirely. Use for byte-identical
    /// reproducibility tests or when debugging a generation bug.
    Off,
}

/// Resolved spec-dec plan — what the adapter actually emits as args.
///
/// `mode == Off` is a sentinel meaning "no flags appended". `Mtp` means
/// "no draft model, MTP draws from the main model's head". `DraftModel`
/// means "`--spec-draft-model <path>` + GPU offload".
#[derive(Debug, Clone, PartialEq)]
pub struct SpecResolved {
    pub mode: SpecMode,
    pub draft_max: u32,
    pub draft_min: u32,
    pub draft_p_min: f32,
    /// Only meaningful when `mode == DraftModel`.
    pub draft_model_path: Option<String>,
    /// Only meaningful when `mode == DraftModel`. `-1` means
    /// "offload all draft layers" — same semantics as `--ngl 99`.
    pub draft_gpu_layers: i32,
    /// Human-readable reason this mode was chosen. Surfaced in logs +
    /// /lf/status so users can tell why MTP didn't kick in.
    pub reason: String,
}

impl SpecResolved {
    /// "Spec-dec disabled" sentinel — emitted whenever a hard gate
    /// (mode=off, MoE force-disable, model lacks MTP under Auto) refuses
    /// to enable a path.
    pub fn off(reason: impl Into<String>) -> Self {
        Self {
            mode: SpecMode::Off,
            draft_max: 0,
            draft_min: 0,
            draft_p_min: 0.0,
            draft_model_path: None,
            draft_gpu_layers: -1,
            reason: reason.into(),
        }
    }
}

// ── Config block ─────────────────────────────────────────────────────────────

/// `[speculative]` section in `~/.lmforge/config.toml`. Every field is
/// `#[serde(default)]` so a user can write `[speculative]\nmode = "off"`
/// and have everything else fall back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeculativeConfig {
    /// Default mode when `lmforge start` doesn't override.
    #[serde(default)]
    pub mode: SpecMode,
    /// Max draft tokens per step. `llama-server` default is 16 — too
    /// aggressive for low-acceptance MoE models; we cap at 4 when MoE
    /// detection fires (see [`resolve`]).
    #[serde(default = "default_draft_max")]
    pub draft_max: u32,
    /// Min draft tokens per step. 0 = adaptive (server picks).
    #[serde(default)]
    pub draft_min: u32,
    /// Probability threshold below which the draft head bails out and
    /// re-enters greedy. Higher = more conservative = lower accept rate
    /// but cheaper rollback. `0.75` matches the b9351 default.
    #[serde(default = "default_draft_p_min")]
    pub draft_p_min: f32,
    /// `--spec-draft-ngl` value for `DraftModel` mode. `-1` = offload all.
    #[serde(default = "default_draft_gpu_layers")]
    pub draft_gpu_layers: i32,
    /// VRAM safety margin (MiB) — if `(free_vram - main_model - mmproj)`
    /// is below this floor, [`resolve`] downgrades to Off to avoid OOM.
    #[serde(default = "default_vram_safety_mib")]
    pub vram_safety_mib: u32,
    /// Optional explicit draft model path for `DraftModel` mode. Falls
    /// back to a curated pair lookup in a future iteration.
    #[serde(default)]
    pub draft_model: Option<String>,
}

fn default_draft_max() -> u32 {
    16
}
fn default_draft_p_min() -> f32 {
    0.75
}
fn default_draft_gpu_layers() -> i32 {
    -1
}
fn default_vram_safety_mib() -> u32 {
    1024
}

impl Default for SpeculativeConfig {
    fn default() -> Self {
        Self {
            mode: SpecMode::Auto,
            draft_max: default_draft_max(),
            draft_min: 0,
            draft_p_min: default_draft_p_min(),
            draft_gpu_layers: default_draft_gpu_layers(),
            vram_safety_mib: default_vram_safety_mib(),
            draft_model: None,
        }
    }
}

// ── Resolver inputs ──────────────────────────────────────────────────────────

/// Per-model inputs that influence the spec-dec plan. Filled by the
/// adapter from `ModelIndex` + a quick filesystem stat. Distinct from
/// the global `SpeculativeConfig` so tests can synthesise minimal cases.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelSpecInputs {
    /// `ModelCapabilities.mtp`. `None` means "not probed / unknown".
    pub mtp: Option<bool>,
    /// True when the model is a Mixture-of-Experts (MoE) architecture.
    /// MoE models have lower draft-acceptance rates because experts vary
    /// per token, so we cap `draft_max` aggressively to keep wasted
    /// compute low.
    pub is_moe: bool,
}

/// Live runtime inputs the resolver consults to decide if spec-dec fits
/// in the remaining VRAM budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct VramBudget {
    pub gpu_vendor: GpuVendor,
    pub free_vram_gb: f32,
    pub model_size_gb: f32,
    pub mmproj_size_gb: f32,
}

// ── The resolver ─────────────────────────────────────────────────────────────

/// Decide what spec-dec arguments `llama-server` should receive.
///
/// Precedence (highest first):
///   1. `LMFORGE_SPECULATIVE_MODE` env override (any of `auto`, `mtp`,
///      `draft-model`, `off`). Lets ops opt out at start time without
///      editing config.
///   2. `cfg.mode`.
///
/// Then the resolved mode is *validated* against the model + hardware:
///   * `Mtp` but `caps.mtp != Some(true)` → fall through to `Off` with
///     a `reason` explaining the mismatch. Adapter logs a one-line warn.
///   * `DraftModel` but no draft path → fall through to `Off` with a reason.
///   * Any path that would OOM → fall through to `Off`.
///   * `Auto` with `caps.mtp == Some(true)` → `Mtp`.
///   * `Auto` with eligible draft pair (S-3) → `DraftModel`.
///   * `Auto` otherwise → `Off`.
///
/// MoE override: when `inputs.is_moe`, `draft_max` is clamped to ≤ 4
/// regardless of the configured value. Single most impactful knob on
/// MoE-spec-dec performance — see plan doc S-2.3.
pub fn resolve(
    inputs: ModelSpecInputs,
    cfg: &SpeculativeConfig,
    budget: VramBudget,
    draft: Option<&crate::engine::draft_pairs::DraftResolveContext>,
) -> SpecResolved {
    let requested = env_override().unwrap_or(cfg.mode);

    let draft_max_effective = if inputs.is_moe {
        cfg.draft_max.min(4)
    } else {
        cfg.draft_max
    };

    if matches!(requested, SpecMode::Off) {
        return SpecResolved::off("spec-dec disabled by config");
    }

    let headroom_gb = vram_headroom_gb(&budget);
    let safety_gb = cfg.vram_safety_mib as f32 / 1024.0;

    match requested {
        SpecMode::Mtp => {
            if inputs.mtp != Some(true) {
                return SpecResolved::off(
                    "MTP requested but model.capabilities.mtp != true — \
                     re-pull the model or stay on mode=off",
                );
            }
            // MTP draws from the same model's internal head — adds a few
            // hundred MiB of activation cache. Be conservative: require
            // safety_gb to be free on GPU.
            if budget.gpu_vendor != GpuVendor::None && headroom_gb < safety_gb {
                return SpecResolved::off(format!(
                    "VRAM headroom {:.2} GB < safety floor {:.2} GB — disabling spec-dec to avoid OOM",
                    headroom_gb, safety_gb
                ));
            }
            SpecResolved {
                mode: SpecMode::Mtp,
                draft_max: draft_max_effective,
                draft_min: cfg.draft_min,
                draft_p_min: cfg.draft_p_min,
                draft_model_path: None,
                draft_gpu_layers: cfg.draft_gpu_layers,
                reason: if inputs.is_moe {
                    "MTP enabled (MoE-conservative draft_max≤4)".to_string()
                } else {
                    "MTP enabled".to_string()
                },
            }
        }
        SpecMode::DraftModel => {
            let draft_path = cfg
                .draft_model
                .clone()
                .or_else(|| draft.map(|d| d.gguf_path.to_string_lossy().into_owned()));
            let Some(path) = draft_path else {
                return SpecResolved::off(
                    "draft-model requested but no draft path configured or paired",
                );
            };
            let draft_size = draft.map(|d| d.draft_size_gb).unwrap_or(0.0);
            if budget.gpu_vendor != GpuVendor::None
                && !vram_fits_draft(&budget, draft_size, cfg.vram_safety_mib)
            {
                return SpecResolved::off(format!(
                    "VRAM headroom insufficient for draft model ({:.2} GB needed incl. safety)",
                    draft_size + cfg.vram_safety_mib as f32 / 1024.0
                ));
            }
            SpecResolved {
                mode: SpecMode::DraftModel,
                draft_max: draft_max_effective,
                draft_min: cfg.draft_min,
                draft_p_min: cfg.draft_p_min,
                draft_model_path: Some(path),
                draft_gpu_layers: cfg.draft_gpu_layers,
                reason: if let Some(ctx) = draft {
                    format!("draft-model pairing enabled ({})", ctx.draft_id)
                } else {
                    "draft-model pairing enabled".to_string()
                },
            }
        }
        SpecMode::Auto => {
            if inputs.mtp == Some(true) {
                if budget.gpu_vendor != GpuVendor::None && headroom_gb < safety_gb {
                    return SpecResolved::off(format!(
                        "mode=auto: VRAM headroom {:.2} GB < safety floor {:.2} GB",
                        headroom_gb, safety_gb
                    ));
                }
                return SpecResolved {
                    mode: SpecMode::Mtp,
                    draft_max: draft_max_effective,
                    draft_min: cfg.draft_min,
                    draft_p_min: cfg.draft_p_min,
                    draft_model_path: None,
                    draft_gpu_layers: cfg.draft_gpu_layers,
                    reason: if inputs.is_moe {
                        "auto → MTP (MoE-conservative draft_max≤4)".to_string()
                    } else {
                        "auto → MTP".to_string()
                    },
                };
            }

            if let Some(ctx) = draft {
                if vram_fits_draft(&budget, ctx.draft_size_gb, cfg.vram_safety_mib) {
                    return SpecResolved {
                        mode: SpecMode::DraftModel,
                        draft_max: draft_max_effective,
                        draft_min: cfg.draft_min,
                        draft_p_min: cfg.draft_p_min,
                        draft_model_path: Some(ctx.gguf_path.to_string_lossy().into_owned()),
                        draft_gpu_layers: cfg.draft_gpu_layers,
                        reason: format!("auto → draft-model ({})", ctx.draft_id),
                    };
                }
                return SpecResolved::off(format!(
                    "auto: draft pair {} configured but VRAM headroom insufficient ({:.2} GB draft + safety)",
                    ctx.draft_id, ctx.draft_size_gb
                ));
            }

            SpecResolved::off("mode=auto: no MTP and no eligible draft pair → spec-dec off")
        }
        SpecMode::Off => unreachable!("matched above"),
    }
}

/// VRAM headroom after the main model + mmproj are loaded. CPU-only
/// hosts return a sentinel positive value so the budget check never
/// trips them — spec-dec on CPU is bottlenecked by main-model speed,
/// not the draft head.
fn vram_headroom_gb(budget: &VramBudget) -> f32 {
    if budget.gpu_vendor == GpuVendor::None {
        return f32::INFINITY;
    }
    (budget.free_vram_gb - budget.model_size_gb - budget.mmproj_size_gb).max(0.0)
}

/// Whether the remaining VRAM can host a draft model on top of the main
/// model already loaded. Used by S-3 draft-pair auto resolution.
pub fn vram_fits_draft(budget: &VramBudget, draft_size_gb: f32, vram_safety_mib: u32) -> bool {
    if budget.gpu_vendor == GpuVendor::None {
        return true;
    }
    let safety_gb = vram_safety_mib as f32 / 1024.0;
    vram_headroom_gb(budget) >= draft_size_gb + safety_gb
}

fn env_override() -> Option<SpecMode> {
    let raw = std::env::var("LMFORGE_SPECULATIVE_MODE").ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(SpecMode::Auto),
        "mtp" => Some(SpecMode::Mtp),
        "draft" | "draft-model" | "draftmodel" => Some(SpecMode::DraftModel),
        "off" | "" => Some(SpecMode::Off),
        _ => None,
    }
}

// ── MoE detection ────────────────────────────────────────────────────────────

/// Cheap, name-driven MoE detection from the model id. Used as a
/// fallback when the catalog doesn't explicitly tag MoE — better
/// (catalog field / GGUF metadata probe) coming as part of S-3.
///
/// Matches substring patterns observed in current shortcuts:
///   * `:next` and `-next` suffixes (Qwen Next family)
///   * `a3b` / `a13b` activation-expert suffixes
///   * `coder:next`, `minimax`, `mixtral`, `qwen3-coder-next`
///
/// All matching is case-insensitive on the lowercased id.
pub fn detect_moe_by_name(model_id: &str) -> bool {
    let lid = model_id.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "minimax",
        "mixtral",
        ":next",
        "-next",
        "qwen3-next",
        "qwen3.5-",
        // Qwen3 MoE shortcuts: `qwen3:30b-a3b`, `qwen3:235b-a22b` etc.
        "a3b",
        "a22b",
        "a13b",
        "a14b",
        "moe",
    ];
    NEEDLES.iter().any(|n| lid.contains(n))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_budget(free_vram_gb: f32, model_gb: f32) -> VramBudget {
        VramBudget {
            gpu_vendor: GpuVendor::Nvidia,
            free_vram_gb,
            model_size_gb: model_gb,
            mmproj_size_gb: 0.0,
        }
    }

    fn cpu_budget() -> VramBudget {
        VramBudget {
            gpu_vendor: GpuVendor::None,
            ..Default::default()
        }
    }

    // ── Auto path ────────────────────────────────────────────────────────

    #[test]
    fn resolve_auto_with_mtp_capable_model_enables_mtp() {
        let cfg = SpeculativeConfig::default();
        let inputs = ModelSpecInputs {
            mtp: Some(true),
            is_moe: false,
        };
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), None);
        assert_eq!(r.mode, SpecMode::Mtp);
        assert_eq!(r.draft_max, 16);
        assert!(r.reason.contains("auto"));
    }

    #[test]
    fn resolve_auto_without_mtp_falls_back_to_off() {
        let cfg = SpeculativeConfig::default();
        let inputs = ModelSpecInputs {
            mtp: Some(false),
            ..Default::default()
        };
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), None);
        assert_eq!(r.mode, SpecMode::Off);
        assert!(r.reason.contains("no MTP and no eligible draft pair"));
    }

    #[test]
    fn resolve_auto_unknown_mtp_falls_back_to_off() {
        let cfg = SpeculativeConfig::default();
        let inputs = ModelSpecInputs::default();
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), None);
        assert_eq!(r.mode, SpecMode::Off);
    }

    // ── MoE override ─────────────────────────────────────────────────────

    #[test]
    fn resolve_moe_caps_draft_max_to_four() {
        let cfg = SpeculativeConfig {
            draft_max: 16,
            ..Default::default()
        };
        let inputs = ModelSpecInputs {
            mtp: Some(true),
            is_moe: true,
        };
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), None);
        assert_eq!(r.mode, SpecMode::Mtp);
        assert_eq!(r.draft_max, 4, "MoE must clamp draft_max ≤ 4");
        assert!(r.reason.contains("MoE"));
    }

    #[test]
    fn resolve_moe_does_not_clamp_below_configured_value() {
        let cfg = SpeculativeConfig {
            draft_max: 2,
            ..Default::default()
        };
        let inputs = ModelSpecInputs {
            mtp: Some(true),
            is_moe: true,
        };
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), None);
        assert_eq!(r.draft_max, 2, "MoE clamp must use min(), not overwrite");
    }

    // ── Forced modes ─────────────────────────────────────────────────────

    #[test]
    fn resolve_forced_off_returns_off() {
        let cfg = SpeculativeConfig {
            mode: SpecMode::Off,
            ..Default::default()
        };
        let r = resolve(
            ModelSpecInputs {
                mtp: Some(true),
                ..Default::default()
            },
            &cfg,
            gpu_budget(16.0, 4.0),
            None,
        );
        assert_eq!(r.mode, SpecMode::Off);
    }

    #[test]
    fn resolve_forced_mtp_without_capability_falls_back_to_off_with_reason() {
        let cfg = SpeculativeConfig {
            mode: SpecMode::Mtp,
            ..Default::default()
        };
        let r = resolve(
            ModelSpecInputs {
                mtp: Some(false),
                ..Default::default()
            },
            &cfg,
            gpu_budget(16.0, 4.0),
            None,
        );
        assert_eq!(r.mode, SpecMode::Off);
        assert!(r.reason.contains("mtp != true"));
    }

    #[test]
    fn resolve_draft_model_requires_explicit_path() {
        let cfg = SpeculativeConfig {
            mode: SpecMode::DraftModel,
            draft_model: None,
            ..Default::default()
        };
        let r = resolve(
            ModelSpecInputs::default(),
            &cfg,
            gpu_budget(16.0, 4.0),
            None,
        );
        assert_eq!(r.mode, SpecMode::Off);
        assert!(r.reason.contains("draft path"));
    }

    #[test]
    fn resolve_auto_with_draft_pair_enables_draft_model() {
        use crate::engine::draft_pairs::DraftResolveContext;
        use std::path::PathBuf;

        let cfg = SpeculativeConfig::default();
        let inputs = ModelSpecInputs {
            mtp: Some(false),
            ..Default::default()
        };
        let draft = DraftResolveContext {
            draft_id: "qwen3:0.6b:4bit".to_string(),
            gguf_path: PathBuf::from("/models/qwen3-0.6b.gguf"),
            draft_size_gb: 0.4,
            note: "test".to_string(),
        };
        let r = resolve(inputs, &cfg, gpu_budget(16.0, 4.0), Some(&draft));
        assert_eq!(r.mode, SpecMode::DraftModel);
        assert!(r.reason.contains("auto → draft-model"));
    }

    #[test]
    fn vram_fits_draft_respects_headroom() {
        let budget = gpu_budget(8.0, 6.5);
        assert!(!vram_fits_draft(&budget, 1.5, 1024));
        assert!(vram_fits_draft(&budget, 0.3, 1024));
    }

    #[test]
    fn resolve_draft_model_with_path_enables_draft_mode() {
        let cfg = SpeculativeConfig {
            mode: SpecMode::DraftModel,
            draft_model: Some("/models/qwen3-1.5b.gguf".to_string()),
            ..Default::default()
        };
        let r = resolve(
            ModelSpecInputs {
                mtp: Some(false), // MTP unrelated to draft-model mode
                ..Default::default()
            },
            &cfg,
            gpu_budget(16.0, 4.0),
            None,
        );
        assert_eq!(r.mode, SpecMode::DraftModel);
        assert_eq!(
            r.draft_model_path.as_deref(),
            Some("/models/qwen3-1.5b.gguf")
        );
    }

    // ── VRAM safety floor ────────────────────────────────────────────────

    #[test]
    fn resolve_vram_below_safety_floor_disables_mtp() {
        let cfg = SpeculativeConfig {
            vram_safety_mib: 1024, // 1.0 GB
            ..Default::default()
        };
        // free 4 GB, model 3.5 GB → 0.5 GB headroom, below 1.0 GB floor.
        let r = resolve(
            ModelSpecInputs {
                mtp: Some(true),
                ..Default::default()
            },
            &cfg,
            gpu_budget(4.0, 3.5),
            None,
        );
        assert_eq!(r.mode, SpecMode::Off);
        assert!(r.reason.contains("headroom"));
    }

    #[test]
    fn resolve_cpu_only_ignores_vram_floor() {
        let cfg = SpeculativeConfig::default();
        let r = resolve(
            ModelSpecInputs {
                mtp: Some(true),
                ..Default::default()
            },
            &cfg,
            cpu_budget(),
            None,
        );
        assert_eq!(r.mode, SpecMode::Mtp);
    }

    // ── MoE name detection ───────────────────────────────────────────────

    #[test]
    fn detect_moe_by_name_hits_known_families() {
        assert!(detect_moe_by_name("qwen3-coder:next"));
        assert!(detect_moe_by_name("qwen3:30b-a3b"));
        assert!(detect_moe_by_name("qwen3:235b-a22b"));
        assert!(detect_moe_by_name("MiniMax-M2:4bit"));
        assert!(detect_moe_by_name("mixtral-8x7b:4bit"));
        assert!(detect_moe_by_name("custom-moe-experiment"));
    }

    #[test]
    fn detect_moe_by_name_rejects_dense_models() {
        assert!(!detect_moe_by_name("qwen3:4b:thinking:4bit"));
        assert!(!detect_moe_by_name("qwen2.5-vl:7b:4bit"));
        assert!(!detect_moe_by_name("llama3.2:8b:4bit"));
    }

    // ── env override ─────────────────────────────────────────────────────

    #[test]
    fn env_override_disabled_when_unset() {
        // SAFETY: process-global env mutation isolated by deterministic name.
        unsafe { std::env::remove_var("LMFORGE_SPECULATIVE_MODE") };
        assert!(env_override().is_none());
    }
}
