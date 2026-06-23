//! Windows backend: RDP host (registry/netsh/net) and client (`mstsc`).

use super::{hidden, run, SystemInfo};
use crate::{current_username, primary_ipv4, Protocol};
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;

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

// ---------------------------------------------------------------------------
// System info
// ---------------------------------------------------------------------------

fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "UNKNOWN".to_string())
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
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|tokens| tokens.len() >= 3 && tokens[0] == value)
        .map(|tokens| tokens[2..].join(" "))
}

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

fn rdp_enabled() -> bool {
    reg_query(
        r"HKLM\System\CurrentControlSet\Control\Terminal Server",
        "fDenyTSConnections",
    )
    .and_then(|s| parse_reg_number(&s))
    .map(|deny| deny == 0)
    .unwrap_or(false)
}

pub fn system_info() -> SystemInfo {
    const CV: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    let product = reg_query(CV, "ProductName").unwrap_or_else(|| "Windows".to_string());
    let build = reg_query(CV, "CurrentBuild")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
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
        username: current_username(),
        os_name,
        os_build,
        arch: cpu_arch(),
        ip: primary_ipv4()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "(no network)".to_string()),
        sharing_enabled: rdp_enabled(),
        elevated: is_elevated(),
    }
}

// ---------------------------------------------------------------------------
// Elevation
// ---------------------------------------------------------------------------

/// `net session` only succeeds as Administrator.
fn is_elevated() -> bool {
    hidden("net")
        .args(["session"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn relaunch_elevated() -> io::Result<()> {
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

// ---------------------------------------------------------------------------
// Sharing (RDP host)
// ---------------------------------------------------------------------------

fn run_lenient(program: &str, args: &[&str], tolerate: &str) -> Result<(), String> {
    match run(program, args) {
        Err(e) if e.to_lowercase().contains(tolerate) => Ok(()),
        other => other,
    }
}

fn enable_remote_desktop() -> Vec<(String, Result<(), String>)> {
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

pub fn share(username: &str, password: &str) -> Vec<(String, Result<(), String>)> {
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

    if !password.is_empty() {
        steps.push((
            format!("Set password for \u{201C}{user}\u{201D}"),
            run("net", &["user", user, password]),
        ));
    }

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
// Connecting (RDP client via mstsc)
// ---------------------------------------------------------------------------

/// Qualify a bare username to a local account on the remote PC (`.\name`).
fn qualify_user(user: &str) -> String {
    if user.is_empty() || user.contains('\\') || user.contains('@') {
        user.to_string()
    } else {
        format!(".\\{user}")
    }
}

fn rdp_target(ip: Ipv4Addr, port: u16) -> String {
    if port == 3389 {
        ip.to_string()
    } else {
        format!("{ip}:{port}")
    }
}

pub fn launch(
    ip: Ipv4Addr,
    port: u16,
    username: &str,
    password: &str,
    fullscreen: bool,
    protocol: Protocol,
) -> io::Result<()> {
    if protocol == Protocol::Vnc {
        return launch_vnc(ip);
    }

    let user = username.trim();
    let qualified = qualify_user(user);
    let target = rdp_target(ip, port);

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
        "authentication level:i:0",
        "enablecredsspsupport:i:1",
        "prompt for credentials:i:0",
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

// ---------------------------------------------------------------------------
// Connecting to a Mac/VNC host (Windows has no built-in VNC client)
// ---------------------------------------------------------------------------

/// First match of `bin` on the PATH, via `where`.
fn where_is(bin: &str) -> Option<String> {
    let out = hidden("where").arg(bin).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
}

/// Find an installed VNC viewer and the args to connect to `ip:5900`.
fn find_vnc_viewer(ip: Ipv4Addr) -> Option<(String, Vec<String>)> {
    // `host::5900` forces an absolute port for TigerVNC / RealVNC / UltraVNC.
    let target = format!("{ip}::5900");
    if let Some(p) = where_is("vncviewer.exe") {
        return Some((p, vec![target]));
    }
    for path in [
        r"C:\Program Files\RealVNC\VNC Viewer\vncviewer.exe",
        r"C:\Program Files\uvnc bvba\UltraVNC\vncviewer.exe",
    ] {
        if Path::new(path).exists() {
            return Some((path.to_string(), vec![target.clone()]));
        }
    }
    // TightVNC uses a different binary and flags.
    let tight_args = vec![format!("-host={ip}"), "-port=5900".to_string()];
    if let Some(p) = where_is("tvnviewer.exe") {
        return Some((p, tight_args));
    }
    let tight = r"C:\Program Files\TightVNC\tvnviewer.exe";
    if Path::new(tight).exists() {
        return Some((tight.to_string(), tight_args));
    }
    None
}

fn launch_vnc(ip: Ipv4Addr) -> io::Result<()> {
    if let Some((bin, args)) = find_vnc_viewer(ip) {
        return Command::new(bin).args(args).spawn().map(|_| ());
    }
    // No viewer installed: open the RealVNC download page (it does native macOS
    // login auth) and tell the user to install it, then retry.
    let _ = Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            "https://www.realvnc.com/connect/download/viewer/windows/",
        ])
        .spawn();
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No VNC viewer found. Opened the RealVNC Viewer download page — install it, \
         then press Connect again.",
    ))
}

// ---------------------------------------------------------------------------
// Remote name (RDP certificate CN)
// ---------------------------------------------------------------------------

/// PowerShell that completes an RDP TLS handshake and prints the server
/// certificate's common name (the remote PC's real computer name).
///
/// The certificate is intentionally *not* validated: RDP hosts present a
/// self-signed cert by default (validation would always fail), the connection
/// carries no credentials or data, and it is closed the instant the name is
/// read. We only read the CN for display — never trusting the channel.
const RDP_HOST_SCRIPT: &str = r#";try{$c=New-Object System.Net.Sockets.TcpClient;$c.Connect($ip,$port);$s=$c.GetStream();$s.ReadTimeout=3000;$s.WriteTimeout=3000;$r=[byte[]](3,0,0,19,14,224,0,0,0,0,0,1,0,8,0,3,0,0,0);$s.Write($r,0,$r.Length);$s.Flush();Start-Sleep -Milliseconds 200;$b=New-Object byte[] 1024;[void]$s.Read($b,0,$b.Length);$cb=[System.Net.Security.RemoteCertificateValidationCallback]{param($x1,$x2,$x3,$x4)$true};$ssl=New-Object System.Net.Security.SslStream($s,$false,$cb);$ssl.AuthenticateAsClient($ip);$crt=[System.Security.Cryptography.X509Certificates.X509Certificate2]$ssl.RemoteCertificate;Write-Output $crt.GetNameInfo('SimpleName',$false);$ssl.Close();$c.Close()}catch{}"#;

pub fn remote_name(ip: Ipv4Addr, protocol: Protocol) -> Option<String> {
    if protocol != Protocol::Rdp {
        return None; // VNC name resolution arrives with mDNS in a later phase.
    }
    let script = format!("$ip='{ip}';$port=3389") + RDP_HOST_SCRIPT;
    let path =
        std::env::temp_dir().join(format!("neardesk_{}.ps1", ip.to_string().replace('.', "_")));
    fs::write(&path, script).ok()?;
    let out = hidden("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&path)
        .output();
    let _ = fs::remove_file(&path);
    let name = String::from_utf8_lossy(&out.ok()?.stdout)
        .trim()
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}
