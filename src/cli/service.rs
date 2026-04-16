use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// Generate and install the LMForge system service (Launchd / Systemd)
pub fn install() -> Result<()> {
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("lmforge"));
    let exe_path = exe.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        install_launchd(&exe_path)?;
    }
    
    #[cfg(target_os = "linux")]
    {
        install_systemd(&exe_path)?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        bail!("Service installation is only supported on macOS and Linux.");
    }

    Ok(())
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd()?;
    }
    
    #[cfg(target_os = "linux")]
    {
        uninstall_systemd()?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        bail!("Service uninstallation is only supported on macOS and Linux.");
    }

    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("Could not find home directory")
}

// --- macOS Launchd ---

static LAUNCHD_LABEL: &str = "com.lmforge.daemon";

fn launchd_plist_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library").join("LaunchAgents").join(format!("{}.plist", LAUNCHD_LABEL)))
}

fn install_launchd(exe_path: &str) -> Result<()> {
    let plist_path = launchd_plist_path()?;
    
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}/.lmforge/logs/daemon.out.log</string>
    <key>StandardErrorPath</key>
    <string>{}/.lmforge/logs/daemon.err.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
</dict>
</plist>"#,
        LAUNCHD_LABEL,
        exe_path,
        home_dir()?.to_string_lossy(),
        home_dir()?.to_string_lossy()
    );

    std::fs::write(&plist_path, plist_content)?;
    
    println!("⚙ Loading macOS Launch Agent...");
    let _ = std::process::Command::new("launchctl")
        .args(["unload", plist_path.to_str().unwrap()])
        .output();
        
    let output = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .output()?;
        
    if !output.status.success() {
        bail!("Failed to load launchd agent: {}", String::from_utf8_lossy(&output.stderr));
    }

    println!("✓ LMForge service installed and started.");
    println!("  It will now start automatically on login.");
    Ok(())
}

fn uninstall_launchd() -> Result<()> {
    let plist_path = launchd_plist_path()?;
    if plist_path.exists() {
        println!("⚙ Unloading macOS Launch Agent...");
        std::process::Command::new("launchctl")
            .args(["unload", plist_path.to_str().unwrap()])
            .output()?;
        std::fs::remove_file(&plist_path)?;
    }
    println!("✓ LMForge service uninstalled.");
    Ok(())
}

// --- Linux Systemd (User) ---

static SYSTEMD_SERVICE: &str = "lmforge.service";

fn systemd_unit_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".config").join("systemd").join("user").join(SYSTEMD_SERVICE))
}

fn install_systemd(exe_path: &str) -> Result<()> {
    let unit_path = systemd_unit_path()?;
    
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Notice we use ~/.lmforge/logs for out/err so it's consistent
    let service_content = format!(
        r#"[Unit]
Description=LMForge LLM Orchestrator
After=network.target

[Service]
Type=simple
ExecStart={} start --foreground
Restart=always
RestartSec=3
Environment="PATH=/usr/local/bin:/usr/bin:/bin"
StandardOutput=append:{}/.lmforge/logs/daemon.out.log
StandardError=append:{}/.lmforge/logs/daemon.err.log

[Install]
WantedBy=default.target
"#,
        exe_path,
        home_dir()?.to_string_lossy(),
        home_dir()?.to_string_lossy()
    );

    std::fs::write(&unit_path, service_content)?;

    println!("⚙ Reloading systemd user daemon...");
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()?;

    println!("⚙ Enabling and starting lmforge service...");
    std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE])
        .output()?;

    println!("✓ LMForge service installed and started.");
    println!("  It will now start automatically on login.");
    println!("  To view logs: journalctl --user -u lmforge.service -f");
    Ok(())
}

fn uninstall_systemd() -> Result<()> {
    let unit_path = systemd_unit_path()?;
    if unit_path.exists() {
        println!("⚙ Stopping and disabling systemd service...");
        std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", SYSTEMD_SERVICE])
            .output()?;
        std::fs::remove_file(&unit_path)?;
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output()?;
    }
    println!("✓ LMForge service uninstalled.");
    Ok(())
}
