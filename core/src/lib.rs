//! NearDesk core: cross-platform LAN discovery, system setup, and remote-desktop
//! launching.
//!
//! Everything here is OS-agnostic; the OS-specific behaviour (sharing, launching
//! a client, system info) lives behind the [`platform`] layer, which dispatches
//! to `platform/{windows,macos,linux}.rs` at compile time. Discovery is
//! protocol-aware: it scans for both **RDP (3389)** and **VNC (5900)** and
//! confirms each with a cheap banner probe.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod platform;
pub use platform::{launch, relaunch_elevated, share, system_info, SystemInfo};

const PROBE_TIMEOUT: Duration = Duration::from_millis(700);

// ---------------------------------------------------------------------------
// Protocols
// ---------------------------------------------------------------------------

/// A remote-desktop protocol NearDesk can discover and launch.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Protocol {
    /// Remote Desktop Protocol (Windows / Linux hosts), port 3389.
    Rdp,
    /// Virtual Network Computing (macOS Screen Sharing / Linux), port 5900.
    Vnc,
}

impl Protocol {
    pub fn port(self) -> u16 {
        match self {
            Protocol::Rdp => 3389,
            Protocol::Vnc => 5900,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Vnc => "VNC",
        }
    }
}

// ---------------------------------------------------------------------------
// Identity / network primitives
// ---------------------------------------------------------------------------

/// The current account name (used as a default username guess). Windows sets
/// `USERNAME`; Unix sets `USER`.
pub fn current_username() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
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

/// All of this machine's LAN IPv4 addresses (excludes loopback / link-local).
fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut ips = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            if let IpAddr::V4(v4) = iface.ip() {
                if !v4.is_link_local() && !ips.contains(&v4) {
                    ips.push(v4);
                }
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

/// Resolve a host name (and its `.local` mDNS form) to IPv4 addresses.
fn resolve_name(name: &str) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for candidate in [name.to_string(), format!("{name}.local")] {
        if let Ok(addrs) = (candidate.as_str(), 0u16).to_socket_addrs() {
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

// ---------------------------------------------------------------------------
// Protocol confirmation (cheap, no auth)
// ---------------------------------------------------------------------------

/// A VNC server speaks first, sending a 12-byte `RFB 003.xxx` banner.
fn confirm_vnc(ip: Ipv4Addr) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(ip), 5900);
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(PROBE_TIMEOUT));
    let mut b = [0u8; 12];
    matches!(s.read(&mut b), Ok(n) if n >= 4 && &b[..4] == b"RFB ")
}

/// An RDP server is silent until it gets an X.224 Connection Request; it then
/// replies with a TPKT header (`03 00 ..`).
fn confirm_rdp(ip: Ipv4Addr) -> bool {
    // TPKT + X.224 Connection Request + RDP negotiation request.
    const X224_CR: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xe0, 0, 0, 0, 0, 0, 0x01, 0, 0x08, 0, 0x03, 0, 0, 0,
    ];
    let addr = SocketAddr::new(IpAddr::V4(ip), 3389);
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = s.set_write_timeout(Some(PROBE_TIMEOUT));
    if s.write_all(&X224_CR).is_err() {
        return false;
    }
    let mut b = [0u8; 32];
    matches!(s.read(&mut b), Ok(n) if n >= 2 && b[0] == 0x03 && b[1] == 0x00)
}

fn confirm_protocol(ip: Ipv4Addr, port: u16) -> Option<Protocol> {
    match port {
        5900 if confirm_vnc(ip) => Some(Protocol::Vnc),
        3389 if confirm_rdp(ip) => Some(Protocol::Rdp),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// A discovered machine: its address, the protocol it speaks, and (if known)
/// its real computer name.
#[derive(Clone)]
pub struct Host {
    pub ip: Ipv4Addr,
    pub protocol: Protocol,
    pub name: Option<String>,
}

impl Host {
    /// Name if known, else the IP as a string.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.ip.to_string())
    }
}

/// Result of a discovery pass.
pub struct Discovery {
    pub hosts: Vec<Host>,
    /// The host the requested name matched, if any.
    pub name_match: Option<Ipv4Addr>,
}

/// Probe a set of `(ip, port)` targets for an open port via a bounded pool.
fn scan_targets(targets: Vec<(Ipv4Addr, u16)>) -> Vec<(Ipv4Addr, u16)> {
    let queue = Arc::new(Mutex::new(targets));
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for _ in 0..256 {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        handles.push(thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some((ip, port)) = next else { break };
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            if TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok() {
                let _ = tx.send((ip, port));
            }
        }));
    }
    drop(tx);
    let found: Vec<_> = rx.iter().collect();
    for h in handles {
        let _ = h.join();
    }
    found
}

/// Confirm the protocol of each open port concurrently.
fn confirm_all(open: Vec<(Ipv4Addr, u16)>) -> Vec<(Ipv4Addr, Protocol)> {
    let queue = Arc::new(Mutex::new(open));
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for _ in 0..64 {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        handles.push(thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some((ip, port)) = next else { break };
            if let Some(p) = confirm_protocol(ip, port) {
                let _ = tx.send((ip, p));
            }
        }));
    }
    drop(tx);
    let v: Vec<_> = rx.iter().collect();
    for h in handles {
        let _ = h.join();
    }
    v
}

/// Browse mDNS/Bonjour for a short window, mapping each advertised IPv4 to its
/// machine name. macOS Screen Sharing advertises `_rfb._tcp`; Avahi advertises
/// `_workstation._tcp`. Best-effort: any failure yields an empty map.
fn mdns_names(window: Duration) -> HashMap<Ipv4Addr, String> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    let mut map = HashMap::new();
    let Ok(daemon) = ServiceDaemon::new() else {
        return map;
    };
    let mut receivers = Vec::new();
    for svc in [
        "_rfb._tcp.local.",
        "_workstation._tcp.local.",
        "_rdp._tcp.local.",
    ] {
        if let Ok(r) = daemon.browse(svc) {
            receivers.push(r);
        }
    }
    let start = Instant::now();
    while start.elapsed() < window {
        for r in &receivers {
            if let Ok(ServiceEvent::ServiceResolved(info)) =
                r.recv_timeout(Duration::from_millis(80))
            {
                let host = info
                    .get_hostname()
                    .trim_end_matches('.')
                    .trim_end_matches(".local")
                    .to_string();
                if host.is_empty() {
                    continue;
                }
                for addr in info.get_addresses() {
                    if let IpAddr::V4(v4) = addr {
                        map.entry(*v4).or_insert_with(|| host.clone());
                    }
                }
            }
        }
    }
    let _ = daemon.shutdown();
    map
}

/// Read each host's real computer name concurrently (OS-specific).
fn resolve_names(hosts: &[Host]) -> HashMap<Ipv4Addr, String> {
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for host in hosts {
        let ip = host.ip;
        let proto = host.protocol;
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            if let Some(name) = platform::remote_name(ip, proto) {
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

/// Discover machines: scan every local /24 for RDP (3389) and VNC (5900),
/// confirm each protocol, then read names.
pub fn discover(name: &str) -> Discovery {
    // Browse mDNS in the background while we port-scan.
    let mdns = thread::spawn(|| mdns_names(Duration::from_millis(1500)));

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
    for (a, b, c) in &bases {
        for d in 1..=254u8 {
            let ip = Ipv4Addr::new(*a, *b, *c, d);
            if local_set.contains(&ip) {
                continue;
            }
            targets.push((ip, 3389u16));
            targets.push((ip, 5900u16));
        }
    }
    let typed = name.trim();
    if !typed.is_empty() {
        for ip in resolve_name(typed) {
            targets.push((ip, 3389));
            targets.push((ip, 5900));
        }
    }

    let open = scan_targets(targets);
    // Collapse to one protocol per host, preferring RDP when both are present.
    let mut protocols: HashMap<Ipv4Addr, Protocol> = HashMap::new();
    for (ip, proto) in confirm_all(open) {
        let cur = protocols.get(&ip).copied();
        if cur.is_none() || (cur == Some(Protocol::Vnc) && proto == Protocol::Rdp) {
            protocols.insert(ip, proto);
        }
    }

    let mut hosts: Vec<Host> = protocols
        .into_iter()
        .map(|(ip, protocol)| Host {
            ip,
            protocol,
            name: None,
        })
        .collect();
    hosts.sort_by_key(|h| h.ip);

    // Names: prefer the RDP certificate CN (OS-specific), fall back to mDNS.
    let cert_names = resolve_names(&hosts);
    let mdns_map = mdns.join().unwrap_or_default();
    for host in &mut hosts {
        host.name = cert_names
            .get(&host.ip)
            .cloned()
            .or_else(|| mdns_map.get(&host.ip).cloned());
    }

    let name_match = if typed.is_empty() {
        None
    } else {
        hosts
            .iter()
            .find(|h| {
                h.name
                    .as_deref()
                    .map(|n| n.eq_ignore_ascii_case(typed))
                    .unwrap_or(false)
            })
            .map(|h| h.ip)
    };

    Discovery { hosts, name_match }
}

/// Choose the most likely target: a name match wins, else the only host.
pub fn pick_target(d: &Discovery) -> Option<Ipv4Addr> {
    if let Some(ip) = d.name_match {
        return Some(ip);
    }
    match d.hosts.as_slice() {
        [only] => Some(only.ip),
        _ => None,
    }
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
