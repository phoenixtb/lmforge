//! Freeing a TCP port without killing processes that aren't ours.
//!
//! Startup and engine spawn reclaim ports held by a crashed predecessor. They
//! used to SIGKILL every PID `lsof -ti :PORT` printed — which includes
//! *clients* connected to the port and processes of another LMForge instance
//! or an unrelated app. On 2026-10-05 a second daemon on a different API port
//! killed the first daemon and its oMLX this way. Now only *listeners* are
//! considered, and only those whose command line references this instance's
//! data or models directory (every engine LMForge spawns gets one of those
//! paths as an argument) are killed. Anything else is reported, not touched.

use std::path::{Path, PathBuf};

use tracing::warn;

/// PIDs with a TCP socket in LISTEN state on `port` (never connected clients).
pub fn listening_pids(port: u16) -> Vec<u32> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("lsof")
            .args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
            .output();
        let Ok(out) = out else { return Vec::new() };
        parse_pid_lines(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(windows)]
    {
        let out = crate::util::subprocess::hidden("netstat")
            .args(["-ano", "-p", "TCP"])
            .output();
        let Ok(out) = out else { return Vec::new() };
        parse_netstat_listeners(&String::from_utf8_lossy(&out.stdout), port)
    }
}

/// Best-effort full command line of `pid`.
pub fn process_command_line(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    #[cfg(windows)]
    {
        let out = crate::util::subprocess::hidden("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// True when `cmdline` belongs to this LMForge instance: it mentions one of
/// `owned_roots` (data dir / models dir, raw or canonical), or its executable
/// basename is one of `owned_exes`.
pub fn is_owned_command(cmdline: &str, owned_roots: &[PathBuf], owned_exes: &[&str]) -> bool {
    let cmd = cmdline.replace('\\', "/");
    let mentions_root = owned_roots.iter().any(|root| {
        let r = root.to_string_lossy().replace('\\', "/");
        let r = r.trim_end_matches('/');
        !r.is_empty() && cmd.contains(r)
    });
    if mentions_root {
        return true;
    }
    let exe = cmd.split_whitespace().next().unwrap_or("");
    let base = exe.rsplit('/').next().unwrap_or(exe);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    owned_exes.iter().any(|e| base.eq_ignore_ascii_case(e))
}

/// `roots` plus their canonical forms (macOS `/tmp` → `/private/tmp`, etc.).
pub fn owned_roots(roots: &[&Path]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for r in roots {
        out.push(r.to_path_buf());
        if let Ok(c) = r.canonicalize()
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// Kill the listeners on `port` that belong to this instance. Returns the
/// foreign listeners that were left alone, as `(pid, command line)`.
pub fn kill_owned_listeners(
    port: u16,
    owned_roots: &[PathBuf],
    owned_exes: &[&str],
) -> Vec<(u32, String)> {
    let mut foreign = Vec::new();
    for pid in listening_pids(port) {
        if pid == std::process::id() {
            continue;
        }
        let cmd = process_command_line(pid).unwrap_or_default();
        if is_owned_command(&cmd, owned_roots, owned_exes) {
            kill_pid(pid);
            warn!(pid, port, cmd = %cmd, "Killed stale LMForge-owned port holder");
        } else {
            warn!(pid, port, cmd = %cmd, "Port held by a process this instance does not own — leaving it alone");
            foreign.push((pid, cmd));
        }
    }
    foreign
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = crate::util::subprocess::hidden("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn parse_pid_lines(s: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = s.lines().filter_map(|l| l.trim().parse().ok()).collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// `netstat -ano -p TCP` rows: `TCP  <local>  <foreign>  <state>  <pid>`.
/// Only LISTENING rows whose *local* address ends in `:port`.
#[cfg_attr(unix, allow(dead_code))]
fn parse_netstat_listeners(s: &str, port: u16) -> Vec<u32> {
    let suffix = format!(":{port}");
    let mut pids: Vec<u32> = s
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            match cols.as_slice() {
                [proto, local, _foreign, state, pid]
                    if proto.eq_ignore_ascii_case("TCP")
                        && state.eq_ignore_ascii_case("LISTENING")
                        && local.ends_with(&suffix) =>
                {
                    pid.parse().ok()
                }
                _ => None,
            }
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/home/u/.lmforge"),
            PathBuf::from("/home/u/.lmforge/models"),
        ]
    }

    #[test]
    fn engine_spawned_from_this_instance_is_owned() {
        let llama = "/home/u/.lmforge/engines/cuda13/llama-server --port 11431 --model /home/u/.lmforge/models/q/q.gguf -ngl 99";
        assert!(is_owned_command(llama, &roots(), &[]));
        let omlx = "/opt/homebrew/bin/python3.11 /opt/homebrew/bin/omlx serve --port 11431 --model-dir /home/u/.lmforge/models";
        assert!(is_owned_command(omlx, &roots(), &[]));
    }

    #[test]
    fn another_instances_engine_is_not_owned() {
        // The 2026-10-05 incident: a test daemon with its own data dir found
        // the installed daemon's oMLX on the port it wanted.
        let other =
            "/opt/homebrew/bin/omlx serve --port 11431 --model-dir /Users/t/.lmforge/models";
        let mine = vec![
            PathBuf::from("/tmp/lf-test"),
            PathBuf::from("/tmp/lf-test/models"),
        ];
        assert!(!is_owned_command(other, &mine, &[]));
    }

    #[test]
    fn unrelated_process_is_not_owned() {
        assert!(!is_owned_command(
            "/usr/bin/python3 -m http.server 11431",
            &roots(),
            &[]
        ));
        assert!(!is_owned_command("", &roots(), &["lmforge"]));
    }

    #[test]
    fn owned_exe_names_match_on_basename_only() {
        assert!(is_owned_command(
            "/home/u/.local/bin/lmforge start --foreground",
            &[],
            &["lmforge"]
        ));
        assert!(is_owned_command(
            r"C:\Users\u\.lmforge\bin\lmforge.exe start",
            &[],
            &["lmforge"]
        ));
        assert!(!is_owned_command("/usr/bin/lmforge-ui", &[], &["lmforge"]));
        assert!(!is_owned_command(
            "/usr/bin/vim /home/x/lmforge",
            &[],
            &["lmforge"]
        ));
    }

    #[test]
    fn windows_paths_compare_with_forward_slashes() {
        let roots = vec![PathBuf::from(r"C:\Users\u\.lmforge")];
        let cmd = r#""C:\Users\u\.lmforge\engines\llama-server.exe" --port 11431"#;
        assert!(is_owned_command(cmd, &roots, &[]));
    }

    #[test]
    fn netstat_parsing_keeps_only_local_listeners_on_the_port() {
        let out = "\
  Proto  Local Address          Foreign Address        State           PID
  TCP    127.0.0.1:11431        0.0.0.0:0              LISTENING       4242
  TCP    127.0.0.1:52000        127.0.0.1:11431        ESTABLISHED     777
  TCP    0.0.0.0:114310         0.0.0.0:0              LISTENING       9
  TCP    [::]:11431             [::]:0                 LISTENING       4242
";
        assert_eq!(parse_netstat_listeners(out, 11431), vec![4242]);
    }

    #[test]
    fn lsof_pid_lines_are_deduplicated() {
        assert_eq!(parse_pid_lines("12\n7\n12\n\n"), vec![7, 12]);
    }
}
