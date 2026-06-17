#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Build NearDesk (release).

.DESCRIPTION
    Requires the Rust toolchain (https://rustup.rs) and the MSVC C++ Build Tools.
    Produces target/release/neardesk.exe.

.PARAMETER Clean
    Remove previous build artifacts first (fresh build).

.PARAMETER Run
    Launch the app after a successful build.

.EXAMPLE
    ./build.ps1
    ./build.ps1 -Clean -Run
#>
[CmdletBinding()]
param(
    [switch]$Clean,
    [switch]$Run
)

$ErrorActionPreference = 'Stop'
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found. Install Rust from https://rustup.rs, then reopen the terminal."
}

if ($Clean) {
    Write-Host 'Cleaning previous build...' -ForegroundColor Cyan
    cargo clean --manifest-path $manifest
}

Write-Host 'Building NearDesk (release)...' -ForegroundColor Cyan
cargo build --release --manifest-path $manifest

$exe = Join-Path $PSScriptRoot 'target/release/neardesk.exe'
if (-not (Test-Path $exe)) {
    throw "Build finished but $exe is missing."
}
Write-Host "`nBuilt: $exe" -ForegroundColor Green

if ($Run) {
    Write-Host 'Launching NearDesk...' -ForegroundColor Cyan
    Start-Process $exe
}
