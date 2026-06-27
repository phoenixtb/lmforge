//! Empirical VRAM calibration cache.
//!
//! The analytic footprint estimate (weights + KV + spec overhead) is only a
//! *prior* — it can't perfectly predict engine-version-specific allocations
//! (unified vs per-slot KV, MTP rs-cache sizing, fragmentation, future model
//! types). So after every successful cold load we measure the real VRAM delta
//! (`free_before - free_after`) and remember it, keyed by the exact runtime
//! signature. The next load of the same configuration budgets on the measured
//! value instead of (or in addition to) the analytic guess.
//!
//! This is the mechanism that makes admission *self-correcting* and
//! engine-agnostic: a new model architecture needs no new estimator code — the
//! first load uses the conservative prior, and every subsequent load uses
//! ground truth.
//!
//! Persisted as a small JSON map under `<data_dir>/engines/vram_calibration.json`.
//! Best-effort: any I/O error degrades to "no calibration" and the analytic
//! prior is used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::engine::adapter::ModelRole;
use crate::engine::speculative::SpecMode;

/// On-disk, in-memory VRAM calibration store.
#[derive(Debug, Default)]
pub struct CalibrationStore {
    path: PathBuf,
    map: HashMap<String, f32>,
}

impl CalibrationStore {
    /// Load the calibration map from `<data_dir>/engines/vram_calibration.json`.
    /// Missing/corrupt file → empty store (the analytic prior carries the load).
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("engines").join("vram_calibration.json");
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, f32>>(&s).ok())
            .unwrap_or_default();
        Self { path, map }
    }

    /// Measured total VRAM (GB) for a signature, if we've successfully loaded
    /// this exact configuration before.
    pub fn get(&self, key: &str) -> Option<f32> {
        self.map.get(key).copied()
    }

    /// Record a measured total for a signature and persist. Keeps the MAX
    /// observed for the signature so a transiently-low reading (another GPU
    /// consumer freeing memory mid-measurement) can never under-budget a later
    /// load. Non-positive measurements are ignored.
    pub fn record(&mut self, key: String, measured_gb: f32) {
        if !(measured_gb.is_finite() && measured_gb > 0.0) {
            return;
        }
        let entry = self.map.entry(key).or_insert(0.0);
        if measured_gb > *entry {
            *entry = measured_gb;
            self.persist();
        }
    }

    fn persist(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.map) {
            let _ = std::fs::write(&self.path, s);
        }
    }
}

/// Build the calibration signature for a load. Includes everything that
/// materially changes the VRAM footprint: model id, effective context, the
/// spec-dec mode actually used, and the role.
pub fn signature(model_id: &str, ctx: u32, spec: SpecMode, role: ModelRole) -> String {
    format!("{model_id}|ctx{ctx}|{spec:?}|{role:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reads_back_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = CalibrationStore::load(tmp.path());
        let key = signature("qwen3:4b", 4096, SpecMode::Mtp, ModelRole::Chat);
        store.record(key.clone(), 7.5);
        assert_eq!(store.get(&key), Some(7.5));
        // Lower reading must not lower the stored max.
        store.record(key.clone(), 5.0);
        assert_eq!(store.get(&key), Some(7.5));
        // Higher reading wins.
        store.record(key.clone(), 8.2);
        assert_eq!(store.get(&key), Some(8.2));
    }

    #[test]
    fn ignores_non_positive() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = CalibrationStore::load(tmp.path());
        let key = signature("m", 4096, SpecMode::Off, ModelRole::Chat);
        store.record(key.clone(), 0.0);
        store.record(key.clone(), -3.0);
        assert_eq!(store.get(&key), None);
    }

    #[test]
    fn persists_across_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let key = signature("m", 2048, SpecMode::Off, ModelRole::Embed);
        {
            let mut store = CalibrationStore::load(tmp.path());
            store.record(key.clone(), 3.3);
        }
        let store = CalibrationStore::load(tmp.path());
        assert_eq!(store.get(&key), Some(3.3));
    }
}
