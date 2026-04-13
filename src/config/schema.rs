use anyhow::{bail, Result};
use tracing::warn;

/// Maximum schema version this binary supports
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Check a file's schema version against what this binary supports.
///
/// - If version == current: OK
/// - If version < current: warn (caller should migrate)
/// - If version > current: error (binary too old)
/// - If version == 0: treat as pre-versioning
pub fn check_version(file_name: &str, file_version: u32, current: u32) -> Result<()> {
    if file_version == current {
        return Ok(());
    }

    if file_version > current {
        bail!(
            "{} schema_version {} is newer than this binary supports (max: {}). \
             Please upgrade LMForge.",
            file_name,
            file_version,
            current
        );
    }

    // file_version < current — needs migration
    if file_version == 0 {
        warn!(
            "{} has no schema_version (pre-versioning). Will migrate to v{}.",
            file_name, current
        );
    } else {
        warn!(
            "{} schema_version {} is older than current ({}). Will migrate.",
            file_name, file_version, current
        );
    }

    // TODO: Run migration chain v{old} → v{old+1} → ... → v{current}
    // For now, accept the file as-is since we only have v1

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_match() {
        assert!(check_version("test.toml", 1, 1).is_ok());
    }

    #[test]
    fn test_version_too_new() {
        let result = check_version("models.json", 3, 2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("newer than this binary supports"));
    }

    #[test]
    fn test_version_old_warns_but_ok() {
        // v0 (pre-versioning) should succeed but warn
        assert!(check_version("config.toml", 0, 1).is_ok());
    }
}
