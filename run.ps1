#!/usr/bin/env pwsh
# Build NearDesk (release) and launch it.
$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'build.ps1') -Run
