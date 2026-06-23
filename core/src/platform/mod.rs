//! The OS-specific layer. Exactly one backend is compiled per target.
//!
//! Each backend (`windows.rs`, `macos.rs`, `linux.rs`) provides the same set of
//! functions; `lib.rs` re-exports the cross-platform ones at the crate root.

use std::process::Command;

/// Build a `Command` whose console window stays hidden on Windows (no-op
/// elsewhere) so the GUI never flashes a terminal.
pub(crate) fn hidden(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Run a console helper, mapping a non-zero exit into a readable error.
#[allow(dead_code)] // used by the Windows backend only
pub(crate) fn run(program: &str, args: &[&str]) -> Result<(), String> {
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

/// Capture a command's trimmed stdout (empty on failure).
#[allow(dead_code)]
pub(crate) fn capture(program: &str, args: &[&str]) -> String {
    hidden(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A human-friendly snapshot of this machine, shown in the "This PC" view.
pub struct SystemInfo {
    pub hostname: String,
    pub username: String,
    /// e.g. "Windows 11 Pro", "macOS 14.5", "Ubuntu 24.04 LTS".
    pub os_name: String,
    /// e.g. "23H2 (build 22631.3447)", build number, or kernel.
    pub os_build: String,
    /// e.g. "x64", "ARM64".
    pub arch: String,
    /// Primary LAN IPv4, or "(no network)".
    pub ip: String,
    /// Is this machine currently shareable (RDP/VNC host enabled)?
    pub sharing_enabled: bool,
    /// Are we running with the privileges needed to change sharing settings?
    pub elevated: bool,
}

#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
mod imp;

pub(crate) use imp::remote_name;
pub use imp::{launch, relaunch_elevated, share, system_info};
