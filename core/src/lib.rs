//! NearDesk core: LAN discovery, system setup, and Remote Desktop launching.
//!
//! Pure `std` only — no third-party crates. Windows-only (shells out to
//! `reg`, `netsh`, `powercfg`, `net`, `mstsc`). All helper processes are
//! launched with a hidden console window so the GUI never flashes a terminal.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

/// CreateProcess flag: don't open a console window for the child.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// Minimal Win32 bindings (in-process — no spawned helper).
#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetDpiForSystem() -> u32;
}

/// The primary monitor's pixel size (falls back to 1920x1080).
fn primary_screen_size() -> (u32, u32) {
    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if w > 0 && h > 0 {
        (w as u32, h as u32)
    } else {
        (1920, 1080)
    }
}

/// The display scale to apply in the session, snapped to a value RDP accepts.
fn desktop_scale_factor() -> u32 {
    let pct = (unsafe { GetDpiForSystem() } as f32 / 96.0 * 100.0).round() as i32;
    [100, 125, 150, 175, 200, 250, 300]
        .into_iter()
        .min_by_key(|v| (v - pct).abs())
        .unwrap_or(100) as u32
}

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

/// The current Windows account name (used as a default username guess).
pub fn current_username() -> String {
    std::env::var("USERNAME").unwrap_or_default()
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

/// All of this machine's LAN IPv4 addresses (excludes loopback / link-local).
fn local_ipv4s() -> Vec<Ipv4Addr> {
    let out = hidden("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -ne '127.0.0.1' -and $_.IPAddress -notlike '169.254.*' }).IPAddress",
        ])
        .output();
    let mut ips = Vec::new();
    if let Ok(o) = out {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Ok(ip) = line.trim().parse::<Ipv4Addr>() {
                ips.push(ip);
            }
        }
    }
    if ips.is_empty() {
        if let Some(ip) = primary_ipv4() {
            ips.push(ip);
        }
    }
    ips
}

/// Scan every /24 this machine is attached to for an open RDP port. Covers
/// Wi-Fi, Ethernet, and VPN interfaces — not just the default-route network.
pub fn scan_subnet(port: u16) -> Vec<Ipv4Addr> {
    let locals = local_ipv4s();
    let local_set: HashSet<Ipv4Addr> = locals.iter().copied().collect();

    let mut bases: Vec<(u8, u8, u8)> = Vec::new();
    for ip in &locals {
        let o = ip.octets();
        let base = (o[0], o[1], o[2]);
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    bases.truncate(8); // guard against a swarm of virtual adapters

    let mut targets = Vec::new();
    for (a, b, c) in bases {
        for d in 1..=254u8 {
            let ip = Ipv4Addr::new(a, b, c, d);
            if !local_set.contains(&ip) {
                targets.push(ip);
            }
        }
    }
    scan_targets(targets, port)
}

/// Probe a set of addresses for an open port using a bounded worker pool.
fn scan_targets(targets: Vec<Ipv4Addr>, port: u16) -> Vec<Ipv4Addr> {
    let queue = Arc::new(Mutex::new(targets));
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();

    for _ in 0..256 {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        handles.push(thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some(ip) = next else { break };
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            if test_rdp(addr, RDP_TIMEOUT) {
                let _ = tx.send(ip);
            }
        }));
    }
    drop(tx);

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
    /// Real computer names of the hosts, read from their RDP certificates.
    pub names: HashMap<Ipv4Addr, String>,
}

/// Discover hosts: scan the subnet, cross-check the requested name, and read
/// every reachable host's real computer name.
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

    let names = resolve_hostnames(&hits, port);
    Discovery {
        hits,
        name_match,
        names,
    }
}

/// Read every host's computer name concurrently.
fn resolve_hostnames(hits: &[Ipv4Addr], port: u16) -> HashMap<Ipv4Addr, String> {
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for &ip in hits {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            if let Some(name) = remote_hostname(ip, port) {
                let _ = tx.send((ip, name));
            }
        }));
    }
    drop(tx);
    let map: HashMap<Ipv4Addr, String> = rx.iter().collect();
    for h in handles {
        let _ = h.join();
    }
    map
}

/// PowerShell that completes an RDP TLS handshake and prints the server
/// certificate's common name — the remote PC's real computer name. `$ip` and
/// `$port` are prepended by `remote_hostname`.
///
/// The certificate is intentionally *not* validated: RDP hosts present a
/// self-signed cert by default (validation would always fail), the connection
/// carries no credentials or data, and it is closed the instant the name is
/// read. We are only reading the CN for display — never trusting the channel.
const RDP_HOST_SCRIPT: &str = r#";try{$c=New-Object System.Net.Sockets.TcpClient;$c.Connect($ip,$port);$s=$c.GetStream();$s.ReadTimeout=3000;$s.WriteTimeout=3000;$r=[byte[]](3,0,0,19,14,224,0,0,0,0,0,1,0,8,0,3,0,0,0);$s.Write($r,0,$r.Length);$s.Flush();Start-Sleep -Milliseconds 200;$b=New-Object byte[] 1024;[void]$s.Read($b,0,$b.Length);$cb=[System.Net.Security.RemoteCertificateValidationCallback]{param($x1,$x2,$x3,$x4)$true};$ssl=New-Object System.Net.Security.SslStream($s,$false,$cb);$ssl.AuthenticateAsClient($ip);$crt=[System.Security.Cryptography.X509Certificates.X509Certificate2]$ssl.RemoteCertificate;Write-Output $crt.GetNameInfo('SimpleName',$false);$ssl.Close();$c.Close()}catch{}"#;

/// Read a host's Windows computer name from its RDP certificate.
pub fn remote_hostname(ip: Ipv4Addr, port: u16) -> Option<String> {
    let script = format!("$ip='{ip}';$port={port}") + RDP_HOST_SCRIPT;
    // Per-host file so concurrent resolves don't clobber each other.
    let path =
        std::env::temp_dir().join(format!("neardesk_{}.ps1", ip.to_string().replace('.', "_")));
    fs::write(&path, script).ok()?;
    let out = hidden("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&path)
        .output();
    let _ = fs::remove_file(&path);
    let out = out.ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
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

/// Run a console helper, treating an error containing `tolerate` as success.
fn run_lenient(program: &str, args: &[&str], tolerate: &str) -> Result<(), String> {
    match run(program, args) {
        Err(e) if e.to_lowercase().contains(tolerate) => Ok(()),
        other => other,
    }
}

/// Create (or update) a local account and allow it to use Remote Desktop.
/// Requires Administrator. Returns one `(step, result)` per action.
pub fn create_rdp_login(username: &str, password: &str) -> Vec<(String, Result<(), String>)> {
    let user = username.trim();
    if user.is_empty() {
        return vec![(
            "Create the sign-in".to_string(),
            Err("Enter a username.".to_string()),
        )];
    }
    if password.is_empty() {
        return vec![(
            "Create the sign-in".to_string(),
            Err("Enter a password.".to_string()),
        )];
    }

    // Create the account, or just reset its password if it already exists.
    let account = match run("net", &["user", user, password, "/add"]) {
        Ok(()) => Ok(()),
        Err(_) => run("net", &["user", user, password]),
    };

    vec![
        (format!("Account \u{201C}{user}\u{201D}"), account),
        (
            "Allow Remote Desktop sign-in".to_string(),
            run_lenient(
                "net",
                &["localgroup", "Remote Desktop Users", user, "/add"],
                "already a member",
            ),
        ),
    ]
}

/// Ensure clipboard redirection is allowed by the RDP host.
fn allow_clipboard() -> Result<(), String> {
    run(
        "reg",
        &[
            "add",
            r"HKLM\System\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp",
            "/v",
            "fDisableClip",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ],
    )
}

/// Share this PC: enable Remote Desktop, allow clipboard sharing, and grant the
/// signed-in account administrator + Remote Desktop access. The account's
/// existing Windows password is used to connect; a password is only set when
/// `password` is non-empty (for an account that currently has none).
/// Requires Administrator.
pub fn share_this_pc(username: &str, password: &str) -> Vec<(String, Result<(), String>)> {
    let mut steps = enable_remote_desktop();
    steps.push(("Allow clipboard sharing".to_string(), allow_clipboard()));

    let user = username.trim();
    if user.is_empty() {
        steps.push((
            "Set up the sign-in".to_string(),
            Err("No Windows account name was found.".to_string()),
        ));
        return steps;
    }

    steps.push((
        "Grant administrator access".to_string(),
        run_lenient(
            "net",
            &["localgroup", "Administrators", user, "/add"],
            "already a member",
        ),
    ));
    steps.push((
        "Allow Remote Desktop sign-in".to_string(),
        run_lenient(
            "net",
            &["localgroup", "Remote Desktop Users", user, "/add"],
            "already a member",
        ),
    ));

    // The existing Windows password is used by default; only set one if given
    // (for a local account that currently has no password at all).
    if !password.is_empty() {
        steps.push((
            format!("Set password for \u{201C}{user}\u{201D}"),
            run("net", &["user", user, password]),
        ));
    }

    // One profile only: drop the separate "neardesk" account older versions made.
    if !user.eq_ignore_ascii_case("neardesk") {
        steps.push((
            "Remove old NearDesk account".to_string(),
            run_lenient(
                "net",
                &["user", "neardesk", "/delete"],
                "could not be found",
            ),
        ));
    }
    steps
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

/// Qualify a bare username to a local account on the remote PC (`.\name`), so
/// Windows authenticates it against the target — not the client or a domain.
/// Names already carrying a domain (`\`) or a UPN (`@`) are left untouched.
fn qualify_user(user: &str) -> String {
    if user.is_empty() || user.contains('\\') || user.contains('@') {
        user.to_string()
    } else {
        format!(".\\{user}")
    }
}

/// The address passed to `mstsc /v:` and used as the credential target.
fn rdp_target(ip: Ipv4Addr, port: u16) -> String {
    if port == 3389 {
        ip.to_string()
    } else {
        format!("{ip}:{port}")
    }
}

/// Launch Remote Desktop against a host.
///
/// With a `password`, the credential is stored for this host (via `cmdkey`,
/// into Windows Credential Manager) so the session connects with no prompt.
/// With only a `username`, it is pre-filled and Windows asks for the password.
/// With neither, a plain connection is opened.
///
/// Note: Remote Desktop never accepts a Windows Hello PIN — only the account's
/// real password (or, for a Microsoft account, the online email + password).
pub fn launch_rdp(
    ip: Ipv4Addr,
    port: u16,
    username: &str,
    password: &str,
    fullscreen: bool,
) -> std::io::Result<()> {
    let user = username.trim();
    let qualified = qualify_user(user);
    let target = rdp_target(ip, port);

    // With a password, stash the credential so the session connects with no prompt.
    if !user.is_empty() && !password.is_empty() {
        let _ = hidden("cmdkey")
            .arg(format!("/generic:TERMSRV/{target}"))
            .arg(format!("/user:{qualified}"))
            .arg(format!("/pass:{password}"))
            .output();
    }

    let mut rdp = String::new();
    rdp.push_str(&format!("full address:s:{target}\r\n"));
    if !user.is_empty() {
        rdp.push_str(&format!("username:s:{qualified}\r\n"));
    }
    // Match the client monitor so the remote desktop is sharp, not upscaled.
    let (sw, sh) = primary_screen_size();
    rdp.push_str(&format!(
        "screen mode id:i:{}\r\n",
        if fullscreen { 2 } else { 1 }
    ));
    rdp.push_str(&format!("desktopwidth:i:{sw}\r\n"));
    rdp.push_str(&format!("desktopheight:i:{sh}\r\n"));
    rdp.push_str(&format!(
        "desktopscalefactor:i:{}\r\n",
        desktop_scale_factor()
    ));
    rdp.push_str("session bpp:i:32\r\n");
    rdp.push_str("smart sizing:i:1\r\n");
    for line in [
        // Connect silently — no identity/trust prompt (the remote may be headless).
        "authentication level:i:0",
        "enablecredsspsupport:i:1",
        "prompt for credentials:i:0",
        // Share the clipboard only; everything else off avoids the "do you trust" dialog.
        "redirectclipboard:i:1",
        "redirectprinters:i:0",
        "redirectcomports:i:0",
        "redirectsmartcards:i:0",
        "redirectposdevices:i:0",
        "drivestoredirect:s:",
        "devicestoredirect:s:",
        "audiocapturemode:i:0",
        "autoreconnection enabled:i:1",
        "bandwidthautodetect:i:1",
        "networkautodetect:i:1",
    ] {
        rdp.push_str(line);
        rdp.push_str("\r\n");
    }

    let path = std::env::temp_dir().join("neardesk.rdp");
    fs::write(&path, rdp)?;
    Command::new("mstsc").arg(&path).spawn().map(|_| ())
}

/// Remove any saved Remote Desktop credential for a host.
pub fn clear_credential(ip: Ipv4Addr, port: u16) {
    let _ = hidden("cmdkey")
        .arg(format!("/delete:TERMSRV/{}", rdp_target(ip, port)))
        .output();
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
