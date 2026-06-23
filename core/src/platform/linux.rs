//! Linux backend: FreeRDP / Remmina / GNOME Connections as clients; xrdp or
//! GNOME Remote Desktop for hosting (guided in a later phase).

use super::{capture, SystemInfo};
use crate::{current_username, primary_ipv4, Protocol};
use std::io;
use std::net::Ipv4Addr;
use std::process::Command;

fn which(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "ARM64".to_string(),
        other => other.to_string(),
    }
}

fn is_root() -> bool {
    capture("id", &["-u"]) == "0"
}

/// `PRETTY_NAME` from /etc/os-release, e.g. "Ubuntu 24.04 LTS".
fn distro() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".to_string())
}

pub fn system_info() -> SystemInfo {
    let hostname = {
        let h = capture("hostname", &[]);
        if h.is_empty() {
            std::fs::read_to_string("/etc/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            h
        }
    };
    let kernel = capture("uname", &["-r"]);
    SystemInfo {
        hostname,
        username: current_username(),
        os_name: distro(),
        os_build: if kernel.is_empty() {
            "unknown".to_string()
        } else {
            format!("kernel {kernel}")
        },
        arch: arch(),
        ip: primary_ipv4()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "(no network)".to_string()),
        sharing_enabled: false,
        // Treated as ready so the UI shows the (guided) share panel.
        elevated: true,
    }
}

pub fn relaunch_elevated() -> io::Result<()> {
    Ok(())
}

pub fn share(_username: &str, _password: &str) -> Vec<(String, Result<(), String>)> {
    let admin = if is_root() { "" } else { " (needs sudo)" };
    vec![(
        format!("Share this PC{admin}"),
        Err(
            "Hosting from Linux is coming soon. For now enable an RDP host with:  \
             sudo apt install -y xrdp && sudo adduser xrdp ssl-cert && sudo systemctl \
             enable --now xrdp && sudo ufw allow 3389/tcp  (use an Xorg session). \
             Connecting FROM this PC already works once a client is installed."
                .to_string(),
        ),
    )]
}

pub fn launch(
    ip: Ipv4Addr,
    _port: u16,
    username: &str,
    password: &str,
    _fullscreen: bool,
    protocol: Protocol,
) -> io::Result<()> {
    let user = username.trim();
    match protocol {
        Protocol::Rdp => {
            if let Some(bin) = ["xfreerdp3", "xfreerdp"].into_iter().find(|b| which(b)) {
                let mut c = Command::new(bin);
                c.arg(format!("/v:{ip}:3389"))
                    .arg("/cert:ignore")
                    .arg("/dynamic-resolution")
                    .arg("+clipboard");
                if !user.is_empty() {
                    c.arg(format!("/u:{user}"));
                }
                if !password.is_empty() {
                    c.arg(format!("/p:{password}"));
                }
                return c.spawn().map(|_| ());
            }
            let uri = if user.is_empty() {
                format!("rdp://{ip}")
            } else {
                format!("rdp://{user}@{ip}")
            };
            if which("remmina") {
                return Command::new("remmina")
                    .arg("-c")
                    .arg(uri)
                    .spawn()
                    .map(|_| ());
            }
            if which("gnome-connections") {
                return Command::new("gnome-connections")
                    .arg(uri)
                    .spawn()
                    .map(|_| ());
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No RDP client found. Install one with:  sudo apt install -y freerdp3-x11",
            ))
        }
        Protocol::Vnc => {
            let uri = format!("vnc://{ip}");
            if which("remmina") {
                return Command::new("remmina")
                    .arg("-c")
                    .arg(uri)
                    .spawn()
                    .map(|_| ());
            }
            if which("gnome-connections") {
                return Command::new("gnome-connections")
                    .arg(uri)
                    .spawn()
                    .map(|_| ());
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No VNC client found. Install one with:  sudo apt install -y remmina remmina-plugin-vnc",
            ))
        }
    }
}

pub fn remote_name(_ip: Ipv4Addr, _protocol: Protocol) -> Option<String> {
    None
}
