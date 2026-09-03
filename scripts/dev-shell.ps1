<#
.SYNOPSIS
  Load the complete MSVC build environment so cargo can link on this box.

.DESCRIPTION
  rustc's auto-detection here picks the *incomplete* "VS 18 Insiders" MSVC toolchain, whose link
  step fails (LNK1181/LNK1104: missing legacy_stdio_definitions.lib / msvcrt.lib). The complete
  toolchain is the stable VS install. This script locates the latest *stable* VS that ships the C++
  build tools (vswhere excludes prerelease/Insiders by default) and imports its vcvars64 env.

.EXAMPLE
  # Recommended: dot-source once per shell, then use cargo normally.
  . .\scripts\dev-shell.ps1
  cargo run -p scriptor-server

.EXAMPLE
  # One-shot (child shell / CI). Pass the whole command as one string so flags like -p survive.
  powershell -NoProfile -File scripts\dev-shell.ps1 -Run 'cargo run -p scriptor-server'
#>
[CmdletBinding()]
param([string] $Run)

$ErrorActionPreference = 'Stop'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
  throw "vswhere not found at $vswhere - is Visual Studio (or the Build Tools) installed?"
}

$vsPath = & $vswhere -latest -products * `
  -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
  -property installationPath
if (-not $vsPath) {
  throw "No stable Visual Studio install with the C++ build tools (VC.Tools.x86.x64) was found."
}

$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) { throw "vcvars64.bat not found under $vsPath." }

# Import the vcvars environment into the current session.
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
  if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] }
}

if ($Run) {
  Invoke-Expression $Run
  exit $LASTEXITCODE
}

Write-Host "MSVC environment loaded from: $vsPath" -ForegroundColor Green
Write-Host "Dot-source this script ('. .\scripts\dev-shell.ps1') so cargo links in this shell."
