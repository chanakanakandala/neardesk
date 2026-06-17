#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Lint gate: formatting check + clippy with warnings denied.

.DESCRIPTION
    Run this before committing (and in CI). Fails if the code is not formatted
    or clippy reports any warning. Needs the rustfmt and clippy components:
        rustup component add rustfmt clippy
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'

Write-Host 'Checking formatting...' -ForegroundColor Cyan
cargo fmt --all --manifest-path $manifest --check

Write-Host 'Running clippy...' -ForegroundColor Cyan
cargo clippy --all-targets --manifest-path $manifest -- -D warnings

Write-Host "`nAll checks passed." -ForegroundColor Green
