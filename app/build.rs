//! Embed Windows version metadata, an application manifest, and the icon.
//!
//! Unsigned executables with no version info are far more likely to trip
//! antivirus machine-learning heuristics. Proper metadata + a manifest make the
//! binary look like the legitimate application it is.

fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/logo.ico");
        res.set("ProductName", "NearDesk");
        res.set("FileDescription", "NearDesk \u{2014} LAN Remote Desktop");
        res.set("CompanyName", "NearDesk");
        res.set("LegalCopyright", "MIT licensed");
        res.set("OriginalFilename", "neardesk.exe");
        res.set_manifest(MANIFEST);
        if let Err(e) = res.compile() {
            // Don't fail the build if the resource compiler is unavailable.
            println!("cargo:warning=winresource: {e}");
        }
    }
}

#[cfg(windows)]
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="NearDesk.NearDesk" version="0.4.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
    </windowsSettings>
  </application>
</assembly>
"#;
