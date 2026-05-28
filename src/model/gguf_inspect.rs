//! Minimal GGUF inspector — tensor-name lookup only.
//!
//! We do NOT depend on the `gguf` crate (v0.1.2): its `GGMLType` enum
//! predates the K/IQ/BF16/TQ quants used by every modern catalog repo, so
//! tensor parsing fails on anything Unsloth has published in the last 2
//! years. We do not need the full structure — we only need tensor names
//! to detect MTP / nextn layers.
//!
//! Scope: GGUF v2 / v3 little-endian. We deliberately skip metadata
//! decoding (just advance past it) so unknown value types in future
//! revisions do not bork the probe.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

const GGUF_MAGIC: [u8; 4] = *b"GGUF";

/// Per-call read budget on metadata strings — guards against a malformed
/// file claiming an absurd length and OOMing us. Real metadata strings
/// are kilobytes at most.
const MAX_STRING_LEN: u64 = 16 * 1024 * 1024;

/// Hard cap on the number of metadata entries we'll walk through. Real
/// files top out around ~30 entries; 1024 is a generous safety net.
const MAX_METADATA_ENTRIES: u64 = 4096;

/// Same for tensors — Qwen3-Next has ~600 tensors; 1M is impossible.
const MAX_TENSOR_COUNT: u64 = 1_000_000;

/// GGUF metadata value type tags (subset we care about; the rest fall
/// through to a generic byte-skip in `skip_value`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetaType {
    U8 = 0,
    I8 = 1,
    U16 = 2,
    I16 = 3,
    U32 = 4,
    I32 = 5,
    F32 = 6,
    Bool = 7,
    String = 8,
    Array = 9,
    U64 = 10,
    I64 = 11,
    F64 = 12,
}

impl MetaType {
    fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::U8,
            1 => Self::I8,
            2 => Self::U16,
            3 => Self::I16,
            4 => Self::U32,
            5 => Self::I32,
            6 => Self::F32,
            7 => Self::Bool,
            8 => Self::String,
            9 => Self::Array,
            10 => Self::U64,
            11 => Self::I64,
            12 => Self::F64,
            _ => return None,
        })
    }
}

/// Layered MTP resolver for a freshly-pulled model.
///
/// Resolution order (S-1 / S-1.7):
///   1. If `catalog_mtp` is set, it wins — the catalog is the authoritative
///      record because it's been hand-audited (`data/catalogs/gguf.json`).
///   2. Otherwise, probe the largest .gguf file in `model_dir` for MTP
///      tensor names.
///   3. If the probe fails (truncated file, unsupported version), return
///      `None` so the launch path can fall back to spec-dec OFF instead
///      of guessing.
///
/// `model_dir` is the directory where the downloader staged files. We
/// pick the largest `.gguf` file because multi-quant repos sometimes
/// include shards or sidecars (e.g. `mmproj-*.gguf`) we don't want to
/// probe.
pub fn resolve_mtp_for_model(model_dir: &Path, catalog_mtp: Option<bool>) -> Option<bool> {
    if let Some(flag) = catalog_mtp {
        return Some(flag);
    }
    let largest = largest_gguf_in_dir(model_dir)?;
    detect_mtp(&largest)
}

/// Find the largest `.gguf` file under `dir` (non-recursive — model dirs
/// are flat). Returns `None` if no `.gguf` files are present.
fn largest_gguf_in_dir(dir: &Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            continue;
        }
        // Skip multimodal projectors — they aren't the main model.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("mmproj") {
            continue;
        }
        let size = entry.metadata().ok()?.len();
        if best.as_ref().map(|(s, _)| size > *s).unwrap_or(true) {
            best = Some((size, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Inspect a GGUF file and return whether it carries MTP / nextn tensors.
///
/// `Some(true)`  — at least one tensor name matches an MTP-style pattern.
/// `Some(false)` — file parsed successfully, no MTP tensors found.
/// `None`        — file couldn't be parsed (not GGUF, truncated, malformed).
///
/// The orchestrator treats `None` as "unknown" and falls back to the
/// catalog flag for the final decision.
pub fn detect_mtp(gguf_path: &Path) -> Option<bool> {
    let names = read_tensor_names(gguf_path).ok()?;
    Some(names.iter().any(|n| is_mtp_tensor(n)))
}

/// Return all tensor names from a GGUF file. Errors propagate as a
/// generic string so the caller can log without depending on `io::Error`.
pub fn read_tensor_names(gguf_path: &Path) -> Result<Vec<String>, String> {
    let f = File::open(gguf_path).map_err(|e| format!("open: {e}"))?;
    let mut r = BufReader::new(f);

    // Header.
    let mut magic = [0u8; 4];
    read_exact(&mut r, &mut magic)?;
    if magic != GGUF_MAGIC {
        return Err(format!("bad magic: {magic:?}"));
    }
    let version = read_u32(&mut r)?;
    if version < 2 {
        return Err(format!("unsupported gguf version {version}"));
    }

    let tensor_count = read_u64(&mut r)?;
    if tensor_count > MAX_TENSOR_COUNT {
        return Err(format!("absurd tensor_count {tensor_count}"));
    }
    let metadata_kv_count = read_u64(&mut r)?;
    if metadata_kv_count > MAX_METADATA_ENTRIES {
        return Err(format!("absurd metadata count {metadata_kv_count}"));
    }

    // Metadata — skip all values, just advance the cursor.
    for _ in 0..metadata_kv_count {
        let _key = read_string(&mut r)?;
        let vtype = MetaType::from_u32(read_u32(&mut r)?)
            .ok_or_else(|| "unknown metadata value type".to_string())?;
        skip_value(&mut r, vtype)?;
    }

    // Tensor info section.
    let mut names = Vec::with_capacity(tensor_count.min(2048) as usize);
    for _ in 0..tensor_count {
        let name = read_string(&mut r)?;
        let n_dims = read_u32(&mut r)?;
        // Sanity — real tensors have ≤ 4 dims.
        if n_dims > 8 {
            return Err(format!("tensor {name} has {n_dims} dims"));
        }
        for _ in 0..n_dims {
            let _dim = read_u64(&mut r)?;
        }
        let _ggml_type = read_u32(&mut r)?; // intentionally not decoded
        let _offset = read_u64(&mut r)?;
        names.push(name);
    }

    Ok(names)
}

/// Match the tensor-naming conventions used by llama.cpp for speculative-
/// decoding heads. We accept three families:
///
///   * `mtp.*`       — generic Multi-Token Prediction tag.
///   * `nextn.*`     — Qwen3-Next architecture.
///   * `*.nextn.*`   — embedded nextn block on a multi-stack model.
///   * `*.mtp.*`     — same, embedded MTP head.
///
/// We match case-insensitively because some converters emit `MTP` in
/// upper-case (rare, but seen on community quants).
fn is_mtp_tensor(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("mtp.")
        || n.starts_with("nextn.")
        || n.contains(".mtp.")
        || n.contains(".nextn.")
}

// ── Low-level read helpers (little-endian) ───────────────────────────────────

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), String> {
    r.read_exact(buf).map_err(|e| format!("read: {e}"))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, String> {
    let mut b = [0u8; 4];
    read_exact(r, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64, String> {
    let mut b = [0u8; 8];
    read_exact(r, &mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_string<R: Read>(r: &mut R) -> Result<String, String> {
    let len = read_u64(r)?;
    if len > MAX_STRING_LEN {
        return Err(format!("string len {len} > cap"));
    }
    let mut buf = vec![0u8; len as usize];
    read_exact(r, &mut buf)?;
    String::from_utf8(buf).map_err(|e| format!("utf8: {e}"))
}

fn skip_bytes<R: Read + Seek>(r: &mut R, n: u64) -> Result<(), String> {
    r.seek(SeekFrom::Current(n as i64))
        .map_err(|e| format!("seek: {e}"))?;
    Ok(())
}

/// Advance past one metadata value. For scalars we know the width; for
/// String/Array we recurse on the inner shape.
fn skip_value<R: Read + Seek>(r: &mut R, t: MetaType) -> Result<(), String> {
    match t {
        MetaType::U8 | MetaType::I8 | MetaType::Bool => skip_bytes(r, 1),
        MetaType::U16 | MetaType::I16 => skip_bytes(r, 2),
        MetaType::U32 | MetaType::I32 | MetaType::F32 => skip_bytes(r, 4),
        MetaType::U64 | MetaType::I64 | MetaType::F64 => skip_bytes(r, 8),
        MetaType::String => {
            let len = read_u64(r)?;
            if len > MAX_STRING_LEN {
                return Err(format!("string len {len} > cap"));
            }
            skip_bytes(r, len)
        }
        MetaType::Array => {
            let elem_type = MetaType::from_u32(read_u32(r)?)
                .ok_or_else(|| "unknown array elem type".to_string())?;
            let len = read_u64(r)?;
            // Fast path for fixed-width scalars — avoid N recursive calls.
            let width: Option<u64> = match elem_type {
                MetaType::U8 | MetaType::I8 | MetaType::Bool => Some(1),
                MetaType::U16 | MetaType::I16 => Some(2),
                MetaType::U32 | MetaType::I32 | MetaType::F32 => Some(4),
                MetaType::U64 | MetaType::I64 | MetaType::F64 => Some(8),
                _ => None,
            };
            if let Some(w) = width {
                return skip_bytes(r, len.saturating_mul(w));
            }
            for _ in 0..len {
                skip_value(r, elem_type)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    /// Build a synthetic GGUF v3 file with the given tensor names and
    /// zero metadata. Tensor type/dims are fixed to F32 / 1-dim len=1.
    fn synth_gguf(tensor_names: &[&str]) -> NamedTempFile {
        let mut buf = Vec::<u8>::new();
        buf.extend_from_slice(&GGUF_MAGIC);
        buf.extend_from_slice(&3u32.to_le_bytes()); // version
        buf.extend_from_slice(&(tensor_names.len() as u64).to_le_bytes()); // tensor_count
        buf.extend_from_slice(&0u64.to_le_bytes()); // metadata_kv_count
        for name in tensor_names {
            write_string(&mut buf, name);
            buf.extend_from_slice(&1u32.to_le_bytes()); // n_dims
            buf.extend_from_slice(&1u64.to_le_bytes()); // dims[0]
            buf.extend_from_slice(&0u32.to_le_bytes()); // ggml_type
            buf.extend_from_slice(&0u64.to_le_bytes()); // offset
        }
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&buf).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn detect_mtp_recognises_nextn_prefix() {
        let f = synth_gguf(&["token_embd.weight", "nextn.0.norm.weight"]);
        assert_eq!(detect_mtp(f.path()), Some(true));
    }

    #[test]
    fn detect_mtp_recognises_mtp_prefix() {
        let f = synth_gguf(&["blk.0.attn_q.weight", "mtp.head.weight"]);
        assert_eq!(detect_mtp(f.path()), Some(true));
    }

    #[test]
    fn detect_mtp_recognises_embedded_nextn() {
        // Some converters emit the nextn block as a sub-namespace, e.g.
        // `model.layers.0.nextn.weight`. Make sure we catch that.
        let f = synth_gguf(&["model.layers.0.nextn.weight"]);
        assert_eq!(detect_mtp(f.path()), Some(true));
    }

    #[test]
    fn detect_mtp_case_insensitive() {
        let f = synth_gguf(&["MTP.head.weight"]);
        assert_eq!(detect_mtp(f.path()), Some(true));
    }

    #[test]
    fn detect_mtp_returns_false_on_plain_llama() {
        let f = synth_gguf(&[
            "token_embd.weight",
            "blk.0.attn_q.weight",
            "blk.0.attn_k.weight",
            "blk.0.attn_v.weight",
            "output.weight",
        ]);
        assert_eq!(detect_mtp(f.path()), Some(false));
    }

    #[test]
    fn detect_mtp_returns_none_on_garbage() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"not-a-gguf-file").unwrap();
        tmp.flush().unwrap();
        assert_eq!(detect_mtp(tmp.path()), None);
    }

    #[test]
    fn detect_mtp_returns_none_on_missing_file() {
        assert_eq!(detect_mtp(Path::new("/does/not/exist.gguf")), None);
    }

    #[test]
    fn read_tensor_names_returns_all_names_in_order() {
        let names = ["a.weight", "b.weight", "c.weight"];
        let f = synth_gguf(&names);
        let got = read_tensor_names(f.path()).unwrap();
        assert_eq!(got, names);
    }

    // ── resolve_mtp_for_model — layered precedence ───────────────────────────

    fn write_gguf_into(dir: &Path, name: &str, tensor_names: &[&str]) {
        let mut buf = Vec::<u8>::new();
        buf.extend_from_slice(&GGUF_MAGIC);
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&(tensor_names.len() as u64).to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        for n in tensor_names {
            buf.extend_from_slice(&(n.len() as u64).to_le_bytes());
            buf.extend_from_slice(n.as_bytes());
            buf.extend_from_slice(&1u32.to_le_bytes());
            buf.extend_from_slice(&1u64.to_le_bytes());
            buf.extend_from_slice(&0u32.to_le_bytes());
            buf.extend_from_slice(&0u64.to_le_bytes());
        }
        std::fs::write(dir.join(name), buf).unwrap();
    }

    #[test]
    fn resolve_mtp_catalog_flag_wins_over_probe() {
        let dir = tempfile::tempdir().unwrap();
        write_gguf_into(dir.path(), "model.gguf", &["token_embd.weight"]);

        // Catalog says yes — we trust it even if probe would say no.
        assert_eq!(resolve_mtp_for_model(dir.path(), Some(true)), Some(true));
        // Catalog says no — same: catalog wins.
        assert_eq!(resolve_mtp_for_model(dir.path(), Some(false)), Some(false));
    }

    #[test]
    fn resolve_mtp_falls_back_to_probe_when_catalog_silent() {
        let dir = tempfile::tempdir().unwrap();
        write_gguf_into(dir.path(), "model.gguf", &["nextn.0.norm.weight"]);
        assert_eq!(resolve_mtp_for_model(dir.path(), None), Some(true));
    }

    #[test]
    fn resolve_mtp_picks_largest_gguf_and_skips_mmproj() {
        let dir = tempfile::tempdir().unwrap();
        // mmproj sidecar (smaller; should be skipped).
        write_gguf_into(dir.path(), "mmproj-tiny.gguf", &["mtp.head.weight"]);
        // Main model: no MTP tensors, but larger than the sidecar.
        write_gguf_into(
            dir.path(),
            "model-q4_k_m.gguf",
            &[
                "token_embd.weight",
                "blk.0.attn_q.weight",
                "blk.0.attn_k.weight",
                "blk.0.attn_v.weight",
                "blk.0.ffn_gate.weight",
                "blk.0.ffn_up.weight",
                "blk.0.ffn_down.weight",
            ],
        );
        // Probe must look at the main model, not the mmproj — Some(false).
        assert_eq!(resolve_mtp_for_model(dir.path(), None), Some(false));
    }

    #[test]
    fn resolve_mtp_returns_none_when_no_gguf_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "not a model").unwrap();
        assert_eq!(resolve_mtp_for_model(dir.path(), None), None);
    }
}
