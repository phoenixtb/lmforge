//! Subprocess helpers. On Windows, console tools spawned from a windowless
//! process (the hidden daemon, the tray UI) each allocate a visible conhost
//! window unless CREATE_NO_WINDOW is set — the sysinfo poll alone would flash
//! one every 2 seconds. On Unix this is a plain `Command`.

use std::process::Command;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a `Command` that never allocates a visible console window.
pub fn hidden(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Async variant of [`hidden`] for tokio-spawned subprocesses.
pub fn hidden_tokio(program: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}
