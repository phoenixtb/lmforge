use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// Generate and install the LMForge system service.
/// - macOS  → launchd user agent   (~/Library/LaunchAgents/)
/// - Linux  → systemd user unit    (~/.config/systemd/user/)
/// - Windows → Scheduled Task       (runs at logon, no elevation)
pub fn install() -> Result<()> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lmforge"));
    let exe_path = exe.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    install_launchd(&exe_path)?;

    #[cfg(target_os = "linux")]
    install_systemd(&exe_path)?;

    #[cfg(windows)]
    install_scheduled_task(&exe_path)?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("Service installation is not supported on this platform.");

    Ok(())
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    uninstall_launchd()?;

    #[cfg(target_os = "linux")]
    uninstall_systemd()?;

    #[cfg(windows)]
    uninstall_scheduled_task()?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("Service uninstallation is not supported on this platform.");

    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("Could not find home directory")
}

// --- macOS Launchd ---

static LAUNCHD_LABEL: &str = "com.lmforge.daemon";

fn launchd_plist_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", LAUNCHD_LABEL)))
}

fn install_launchd(exe_path: &str) -> Result<()> {
    let plist_path = launchd_plist_path()?;

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create LaunchAgents dir: {}", parent.display()))?;
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

    std::fs::write(&plist_path, plist_content)
        .with_context(|| format!("Cannot write launchd plist: {}", plist_path.display()))?;

    println!("⚙ Loading macOS Launch Agent...");
    let _ = std::process::Command::new("launchctl")
        .args(["unload", plist_path.to_str().unwrap()])
        .output();

    let output = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .output()
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        bail!(
            "Failed to load launchd agent: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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

#[cfg(target_os = "linux")]
static SYSTEMD_SERVICE: &str = "lmforge.service";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SYSTEMD_SERVICE))
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

// --- Windows Scheduled Task ---

#[cfg(windows)]
static WINDOWS_TASK_NAME: &str = "LMForge Daemon";

/// Register a Windows Scheduled Task that runs `lmforge start --foreground`
/// at every user logon. Uses PowerShell's Register-ScheduledTask — no
/// administrator elevation is required (RunLevel = Limited).
#[cfg(windows)]
fn install_scheduled_task(exe_path: &str) -> Result<()> {
    // Build the log directory so the task can redirect output immediately.
    let home = home_dir()?;
    let log_dir = home.join(".lmforge").join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_out = log_dir.join("daemon.out.log");

    let ps_script = format!(
        r#"
$action  = New-ScheduledTaskAction `
    -Execute "{exe}" `
    -Argument "start --foreground"
$trigger = New-ScheduledTaskTrigger -AtLogon
$settings = New-ScheduledTaskSettingsSet `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero)
$principal = New-ScheduledTaskPrincipal `
    -UserId "$env:USERNAME" `
    -RunLevel Limited `
    -LogonType Interactive
Register-ScheduledTask `
    -TaskName "{task}" `
    -Action $action `
    -Trigger $trigger `
    -Settings $settings `
    -Principal $principal `
    -Force | Out-Null
Start-ScheduledTask -TaskName "{task}"
"#,
        exe = exe_path.replace('"', r#"\""#),
        task = WINDOWS_TASK_NAME,
    );

    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .output()
        .context("Failed to run PowerShell")?;

    if !out.status.success() {
        bail!(
            "Failed to register scheduled task:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    println!("✓ LMForge scheduled task registered and started.");
    println!("  It will now start automatically at logon.");
    println!("  Logs: {}", log_out.display());
    Ok(())
}

#[cfg(windows)]
fn uninstall_scheduled_task() -> Result<()> {
    // Stop first (ignore error if not running)
    std::process::Command::new("taskkill")
        .args(["/F", "/IM", "lmforge.exe"])
        .output()
        .ok();

    let ps = format!(
        r#"Unregister-ScheduledTask -TaskName "{}" -Confirm:$false"#,
        WINDOWS_TASK_NAME
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .output()
        .context("Failed to run PowerShell")?;

    println!("✓ LMForge scheduled task removed.");
    Ok(())
}

// ── Service control (start / stop / status) ───────────────────────────────────

pub fn service_start() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchd_plist_path()?;
        if !plist_path.exists() {
            bail!("Service not installed. Run `lmforge service install` first.");
        }
        let out = std::process::Command::new("launchctl")
            .args(["start", LAUNCHD_LABEL])
            .output()?;
        if out.status.success() {
            println!("✓ LMForge service started.");
        } else {
            bail!("{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let unit_path = systemd_unit_path()?;
        if !unit_path.exists() {
            bail!("Service not installed. Run `lmforge service install` first.");
        }
        std::process::Command::new("systemctl")
            .args(["--user", "start", SYSTEMD_SERVICE])
            .status()?;
        println!("✓ LMForge service started.");
    }

    #[cfg(windows)]
    {
        let out = std::process::Command::new("schtasks")
            .args(["/Run", "/TN", WINDOWS_TASK_NAME])
            .output()?;
        if out.status.success() {
            println!("✓ LMForge scheduled task started.");
        } else {
            bail!("{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("Service control is not supported on this platform.");

    Ok(())
}

pub fn service_stop() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("launchctl")
            .args(["stop", LAUNCHD_LABEL])
            .output()?;
        println!("✓ LMForge service stopped.");
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .args(["--user", "stop", SYSTEMD_SERVICE])
            .status()?;
        println!("✓ LMForge service stopped.");
    }

    #[cfg(windows)]
    {
        // End any running lmforge.exe processes (task stays registered)
        std::process::Command::new("taskkill")
            .args(["/F", "/IM", "lmforge.exe"])
            .output()
            .ok();
        println!("✓ LMForge process terminated.");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("Service control is not supported on this platform.");

    Ok(())
}

pub fn service_status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let installed = launchd_plist_path().map(|p| p.exists()).unwrap_or(false);
        println!(
            "  Service file : {}",
            if installed {
                "installed ✓"
            } else {
                "not installed"
            }
        );
        if installed {
            let out = std::process::Command::new("launchctl")
                .args(["list", LAUNCHD_LABEL])
                .output()?;
            let info = String::from_utf8_lossy(&out.stdout);
            let running = info.contains("\"PID\"") && !info.contains("\"PID\" = 0;");
            println!(
                "  launchd      : {}",
                if running {
                    "running ✓"
                } else {
                    "not running"
                }
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        let installed = systemd_unit_path().map(|p| p.exists()).unwrap_or(false);
        println!(
            "  Service file : {}",
            if installed {
                "installed ✓"
            } else {
                "not installed"
            }
        );
        if installed {
            let out = std::process::Command::new("systemctl")
                .args(["--user", "is-active", SYSTEMD_SERVICE])
                .output()?;
            let active = String::from_utf8_lossy(&out.stdout).trim() == "active";
            println!(
                "  systemd      : {}",
                if active {
                    "active (running) ✓"
                } else {
                    "inactive"
                }
            );
        }
    }

    #[cfg(windows)]
    {
        let out = std::process::Command::new("schtasks")
            .args(["/Query", "/TN", WINDOWS_TASK_NAME, "/FO", "LIST"])
            .output()
            .unwrap_or_else(|_| std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            });
        let info = String::from_utf8_lossy(&out.stdout);
        let installed = out.status.success();
        println!(
            "  Scheduled task: {}",
            if installed {
                "registered ✓"
            } else {
                "not registered"
            }
        );
        if installed {
            let running = info.contains("Running");
            println!(
                "  Task status  : {}",
                if running {
                    "running ✓"
                } else {
                    "not running"
                }
            );
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    println!("  Service management is not available on this platform.");

    // Always show live daemon health — use raw TCP, works on all platforms.
    println!();
    let health_ok = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:11430".parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok();
    println!(
        "  Daemon API   : {}",
        if health_ok {
            "reachable at http://127.0.0.1:11430 ✓"
        } else {
            "not reachable"
        }
    );

    Ok(())
}
