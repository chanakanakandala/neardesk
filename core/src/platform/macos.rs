//! macOS backend: built-in Screen Sharing (`open vnc://`) as the client, FreeRDP
//! or the `rdp://` scheme for Windows/Linux hosts, and guided host setup.
//!
//! macOS cannot be made an unattended RDP/VNC host: Apple's TCC gate means the
//! user must approve Screen Recording once (see `share`). Connecting *from* a
//! Mac works with no extra setup for VNC hosts.

use super::{capture, run, SystemInfo};
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

pub fn system_info() -> SystemInfo {
    let hostname = {
        let n = capture("scutil", &["--get", "ComputerName"]);
        if n.is_empty() {
            capture("hostname", &[])
        } else {
            n
        }
    };
    let product = capture("sw_vers", &["-productVersion"]); // e.g. "14.5"
    let build = capture("sw_vers", &["-buildVersion"]);
    SystemInfo {
        hostname,
        username: current_username(),
        os_name: if product.is_empty() {
            "macOS".to_string()
        } else {
            format!("macOS {product}")
        },
        os_build: if build.is_empty() {
            "unknown".to_string()
        } else {
            build
        },
        arch: arch(),
        ip: primary_ipv4()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "(no network)".to_string()),
        // Reliable detection + enabling is a later (guided) phase.
        sharing_enabled: false,
        // Treated as ready so the UI shows the (guided) share panel, not a
        // Windows-style "Restart as Administrator" prompt.
        elevated: true,
    }
}

pub fn relaunch_elevated() -> io::Result<()> {
    // Hosting/elevation flow on macOS arrives in a later phase.
    Ok(())
}

const KICKSTART: &str =
    "/System/Library/CoreServices/RemoteManagement/ARDAgent.app/Contents/Resources/kickstart";

/// Share this Mac over VNC. Experimental and guided: Apple requires a one-time
/// Screen Recording approval that no app can grant, so this sets the password
/// and starts the service (one admin prompt), then opens the consent pane.
pub fn share(_username: &str, password: &str) -> Vec<(String, Result<(), String>)> {
    let mut steps = Vec::new();

    if password.is_empty() {
        steps.push((
            "Set a password".to_string(),
            Err("Enter a short password (max 8 characters) to connect with.".to_string()),
        ));
    } else {
        // Legacy VNC auth is limited to 8 effective characters.
        let pw: String = password.chars().take(8).collect();
        // One privileged step (a single GUI admin prompt) sets the VNC password
        // and starts the built-in Screen Sharing service on port 5900.
        let inner = format!(
            "{KICKSTART} -configure -clientopts -setvnclegacy -vnclegacy yes -setvncpw -vncpw {pw} ; \
             launchctl enable system/com.apple.screensharing ; \
             launchctl bootstrap system /System/Library/LaunchDaemons/com.apple.screensharing.plist"
        );
        let osa = format!(
            "do shell script \"{}\" with administrator privileges",
            inner.replace('\\', "\\\\").replace('"', "\\\"")
        );
        steps.push((
            "Enable Screen Sharing and set the password".to_string(),
            run("osascript", &["-e", &osa]),
        ));
    }

    // The one thing only a human can do: grant Screen Recording in Settings.
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
    steps.push((
        "Allow Screen Recording (one-time click in Settings)".to_string(),
        Err(
            "Opened System Settings \u{2192} Privacy & Security \u{2192} Screen \
             Recording. Turn it on for Screen Sharing — Apple requires this click; it \
             can't be scripted."
                .to_string(),
        ),
    ));
    steps
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
        Protocol::Vnc => {
            // Built-in Screen Sharing; credentials may be embedded in the URL.
            let mut url = String::from("vnc://");
            if !user.is_empty() {
                url.push_str(user);
                if !password.is_empty() {
                    url.push(':');
                    url.push_str(password);
                }
                url.push('@');
            }
            url.push_str(&ip.to_string());
            Command::new("open").arg(url).spawn().map(|_| ())
        }
        Protocol::Rdp => {
            // FreeRDP can pre-fill the password; the rdp:// scheme cannot.
            let freerdp = ["sdl-freerdp", "xfreerdp"].into_iter().find(|b| which(b));
            if let Some(bin) = freerdp {
                let mut c = Command::new(bin);
                c.arg(format!("/v:{ip}:3389")).arg("/cert:ignore");
                if !user.is_empty() {
                    c.arg(format!("/u:{user}"));
                }
                if !password.is_empty() {
                    c.arg(format!("/p:{password}"));
                }
                return c.spawn().map(|_| ());
            }
            // Fall back to Microsoft's "Windows App" via the rdp:// scheme.
            let mut url = format!("rdp://full%20address=s:{ip}:3389");
            if !user.is_empty() {
                url.push_str(&format!("&username=s:{user}"));
            }
            match Command::new("open").arg(&url).status() {
                Ok(s) if s.success() => Ok(()),
                _ => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No RDP client found. Install one with:  brew install freerdp",
                )),
            }
        }
    }
}

pub fn remote_name(_ip: Ipv4Addr, _protocol: Protocol) -> Option<String> {
    // mDNS-based name resolution is a later phase; show the IP for now.
    None
}
