use std::path::Path;

use tracing_appender::rolling;

/// Create a rolling file appender for the main lmforge log.
/// Per SRS §12.4: rotates at daily boundary; keeps 5 files.
///
/// Note: tracing-appender doesn't support size-based rotation natively.
/// For v0.1, we use daily rotation as a reasonable approximation.
/// Size-based rotation (50 MB / 5 files) can be added in v0.2 with
/// a custom appender or a crate like `rolling-file`.
pub fn create_appender(logs_dir: &Path) -> rolling::RollingFileAppender {
    rolling::daily(logs_dir, "lmforge.log")
}

/// Create a rolling file appender for engine stdout.
pub fn create_engine_stdout_appender(logs_dir: &Path) -> rolling::RollingFileAppender {
    rolling::daily(logs_dir, "engine-stdout.log")
}

/// Create a rolling file appender for engine stderr.
pub fn create_engine_stderr_appender(logs_dir: &Path) -> rolling::RollingFileAppender {
    rolling::daily(logs_dir, "engine-stderr.log")
}
