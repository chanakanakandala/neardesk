//! NearDesk core: LAN discovery, system setup, and Remote Desktop launching.
//!
//! Pure `std` only — no third-party crates. Windows-only (shells out to
//! `reg`, `netsh`, `powercfg`, `net`, `mstsc`). All helper processes are
//! launched with a hidden console window so the GUI never flashes a terminal.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// CreateProcess flag: don't open a console window for the child.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RDP_TIMEOUT: Duration = Duration::from_millis(700);

/// Build a `Command` whose console window stays hidden.
fn hidden(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Run a console helper, mapping a non-zero exit into a readable error.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    match hidden(program).args(args).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr);
            let msg = msg.trim();
            if msg.is_empty() {
                Err(format!("{program} exited with status {}", o.status))
            } else {
                Err(msg.to_string())
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Identity / network primitives
// ---------------------------------------------------------------------------

/// This machine's name (Windows `COMPUTERNAME`).
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "UNKNOWN".to_string())
}

/// Best-guess primary LAN IPv4 (via a connect-less UDP socket).
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

/// Is the RDP (or given) port open on this address?
pub fn test_rdp(addr: SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Resolve a host name (and its `.local` mDNS form) to IPv4 addresses.
pub fn resolve_name(name: &str, port: u16) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for candidate in [name.to_string(), format!("{name}.local")] {
        if let Ok(addrs) = (candidate.as_str(), port).to_socket_addrs() {
            for a in addrs {
                if let IpAddr::V4(v4) = a.ip() {
                    if !out.contains(&v4) {
                        out.push(v4);
                    }
                }
            }
        }
    }
    out
}

/// Concurrently probe every host on the local /24 for an open RDP port.
pub fn scan_subnet(port: u16) -> Vec<Ipv4Addr> {
    let Some(local) = primary_ipv4() else {
        return Vec::new();
    };
    let o = local.octets();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();

    for i in 1..=254u8 {
        let ip = Ipv4Addr::new(o[0], o[1], o[2], i);
        if ip == local {
            continue;
        }
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            if test_rdp(addr, RDP_TIMEOUT) {
                let _ = tx.send(ip);
            }
        }));
    }
    drop(tx); // close channel once every worker holds its own clone

    let mut found: Vec<Ipv4Addr> = rx.iter().collect();
    for h in handles {
        let _ = h.join();
    }
    found.sort();
    found.dedup();
    found
}

/// Result of a discovery pass.
pub struct Discovery {
    /// All reachable RDP hosts on the subnet (plus any name-resolved host).
    pub hits: Vec<Ipv4Addr>,
    /// The reachable host the requested name resolved to, if any.
    pub name_match: Option<Ipv4Addr>,
}

/// Discover the target: scan the subnet and cross-check the requested name.
pub fn discover(name: &str, port: u16) -> Discovery {
    let mut hits = scan_subnet(port);
    let mut name_match = None;

    if !name.trim().is_empty() {
        for ip in resolve_name(name.trim(), port) {
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            if test_rdp(addr, RDP_TIMEOUT) {
                if !hits.contains(&ip) {
                    hits.push(ip);
                }
                if name_match.is_none() {
                    name_match = Some(ip);
                }
            }
        }
    }
    hits.sort();
    hits.dedup();
    Discovery { hits, name_match }
}

/// Choose the most likely target from a discovery result: a name match wins,
/// otherwise the sole reachable host (if there is exactly one).
pub fn pick_target(d: &Discovery) -> Option<Ipv4Addr> {
    if let Some(ip) = d.name_match {
        return Some(ip);
    }
    match d.hits.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Sharing this PC — enable Remote Desktop (requires Administrator)
// ---------------------------------------------------------------------------

/// Are we running elevated? (`net session` only succeeds as admin.)
pub fn is_elevated() -> bool {
    hidden("net")
        .args(["session"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Relaunch this executable elevated via UAC. Caller should exit afterwards.
pub fn relaunch_elevated() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    hidden("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Start-Process -FilePath '{}' -Verb RunAs", exe.display()),
        ])
        .spawn()?;
    Ok(())
}

/// Apply every host-side setting. Returns one `(step, result)` per action so
/// the UI can show a per-step checklist.
pub fn enable_remote_desktop() -> Vec<(String, Result<(), String>)> {
    vec![
        (
            "Enable Remote Desktop".to_string(),
            run(
                "reg",
                &[
                    "add",
                    r"HKLM\System\CurrentControlSet\Control\Terminal Server",
                    "/v",
                    "fDenyTSConnections",
                    "/t",
                    "REG_DWORD",
                    "/d",
                    "0",
                    "/f",
                ],
            ),
        ),
        (
            "Require Network Level Authentication".to_string(),
            run(
                "reg",
                &[
                    "add",
                    r"HKLM\System\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp",
                    "/v",
                    "UserAuthentication",
                    "/t",
                    "REG_DWORD",
                    "/d",
                    "1",
                    "/f",
                ],
            ),
        ),
        (
            "Open firewall: Remote Desktop".to_string(),
            run(
                "netsh",
                &[
                    "advfirewall",
                    "firewall",
                    "set",
                    "rule",
                    "group=remote desktop",
                    "new",
                    "enable=Yes",
                ],
            ),
        ),
        (
            "Open firewall: Network Discovery".to_string(),
            run(
                "netsh",
                &[
                    "advfirewall",
                    "firewall",
                    "set",
                    "rule",
                    "group=Network Discovery",
                    "new",
                    "enable=Yes",
                ],
            ),
        ),
        (
            "Disable sleep on AC power".to_string(),
            run("powercfg", &["/change", "standby-timeout-ac", "0"]),
        ),
        (
            "Disable hibernate on AC power".to_string(),
            run("powercfg", &["/change", "hibernate-timeout-ac", "0"]),
        ),
    ]
}

// ---------------------------------------------------------------------------
// System information (shown in the "This PC" view)
// ---------------------------------------------------------------------------

/// A human-friendly snapshot of this machine.
pub struct SystemInfo {
    pub hostname: String,
    pub username: String,
    /// e.g. "Windows 11 Pro".
    pub os_name: String,
    /// e.g. "23H2 (build 22631.3447)".
    pub os_build: String,
    /// e.g. "x64".
    pub arch: String,
    /// Primary LAN IPv4, or "(no network)".
    pub ip: String,
    pub rdp_enabled: bool,
    pub elevated: bool,
}

/// Read a single registry value via `reg query`, returning its data.
fn reg_query(path: &str, value: &str) -> Option<String> {
    let out = hidden("reg")
        .args(["query", path, "/v", value])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Each value line is: <ValueName> <REG_TYPE> <data...>
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|tokens| tokens.len() >= 3 && tokens[0] == value)
        .map(|tokens| tokens[2..].join(" "))
}

/// Parse a `reg` value that may be `0x..` hex or plain decimal.
fn parse_reg_number(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

fn cpu_arch() -> String {
    match std::env::var("PROCESSOR_ARCHITECTURE").as_deref() {
        Ok("AMD64") => "x64".to_string(),
        Ok("ARM64") => "ARM64".to_string(),
        Ok("x86") => "x86".to_string(),
        Ok(other) => other.to_string(),
        Err(_) => "unknown".to_string(),
    }
}

/// Is Remote Desktop currently enabled on this PC?
pub fn rdp_enabled() -> bool {
    reg_query(
        r"HKLM\System\CurrentControlSet\Control\Terminal Server",
        "fDenyTSConnections",
    )
    .and_then(|s| parse_reg_number(&s))
    .map(|deny| deny == 0)
    .unwrap_or(false)
}

/// Gather a snapshot of this machine for display.
pub fn system_info() -> SystemInfo {
    const CV: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    let product = reg_query(CV, "ProductName").unwrap_or_else(|| "Windows".to_string());
    let build = reg_query(CV, "CurrentBuild")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    // The registry still reports "Windows 10" on Windows 11; build >= 22000 is 11.
    let os_name = if build >= 22000 && product.contains("Windows 10") {
        product.replacen("Windows 10", "Windows 11", 1)
    } else {
        product
    };

    let display = reg_query(CV, "DisplayVersion").unwrap_or_default();
    let ubr = reg_query(CV, "UBR")
        .and_then(|s| parse_reg_number(&s))
        .unwrap_or(0);
    let os_build = match (display.is_empty(), build) {
        (_, 0) => "unknown".to_string(),
        (true, b) => format!("build {b}.{ubr}"),
        (false, b) => format!("{display} (build {b}.{ubr})"),
    };

    SystemInfo {
        hostname: computer_name(),
        username: std::env::var("USERNAME").unwrap_or_else(|_| "unknown".to_string()),
        os_name,
        os_build,
        arch: cpu_arch(),
        ip: primary_ipv4()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "(no network)".to_string()),
        rdp_enabled: rdp_enabled(),
        elevated: is_elevated(),
    }
}

// ---------------------------------------------------------------------------
// Connecting to another PC
// ---------------------------------------------------------------------------

/// Launch the Windows Remote Desktop client against a host.
pub fn launch_mstsc(ip: Ipv4Addr, fullscreen: bool) -> std::io::Result<()> {
    let mut c = Command::new("mstsc");
    c.arg(format!("/v:{ip}"));
    if fullscreen {
        c.arg("/f");
    } else {
        c.arg("/w:1280");
        c.arg("/h:800");
    }
    c.spawn().map(|_| ())
}

// ---------------------------------------------------------------------------
// Tiny key=value config stored next to the executable
// ---------------------------------------------------------------------------

pub fn config_path() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("neardesk.conf")
}

pub fn load_config() -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(s) = fs::read_to_string(config_path()) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    m
}

pub fn save_config(m: &HashMap<String, String>) {
    let mut s = String::from("# NearDesk settings\n");
    for (k, v) in m {
        s.push_str(&format!("{k}={v}\n"));
    }
    let _ = fs::write(config_path(), s);
}
