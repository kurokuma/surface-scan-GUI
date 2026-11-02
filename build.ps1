<#
.SYNOPSIS
Build Web Service Fingerprint using Cargo.lock.
.EXAMPLE
.\build.ps1
.EXAMPLE
.\build.ps1 -Target x86_64-pc-windows-msvc -Check -Offline
.NOTES
No Rust targets, toolchains or system packages are installed by this script.
#>
[CmdletBinding()]
param(
    [ValidatePattern('^[a-zA-Z0-9_][a-zA-Z0-9_.-]*$')]
    [string]$Target,
    [switch]$Development,
    [switch]$Check,
    [switch]$Offline,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
if ($Help) {
    Write-Output @'
Usage: .\build.ps1 [-Target TRIPLE] [-Development] [-Check] [-Offline]

  -Target TRIPLE  Rust target triple (default: rustc host)
  -Development   Use the debug profile instead of release
  -Check         Run cargo check; do not produce an executable
  -Offline       Do not access the network
  -Help          Show this help

All builds use Cargo.lock (--locked) and an explicit --target.
Install target std libraries, a linker and native dependencies separately.
'@
    return
}

$cargoCommand = Get-Command cargo -CommandType Application -ErrorAction Stop
$rustCommand = Get-Command rustc -CommandType Application -ErrorAction Stop
$buildTarget = $Target
if (-not $buildTarget) {
    $rustVersion = & $rustCommand.Source -vV
    if ($LASTEXITCODE -ne 0) { throw "rustc failed (exit $LASTEXITCODE)." }
    $hostLine = $rustVersion | Where-Object { $_ -match '^host: ' } | Select-Object -First 1
    if (-not $hostLine) { throw 'Could not determine the rustc host target.' }
    $buildTarget = ($hostLine -replace '^host:\s*', '').Trim()
}

$buildAction = if ($Check) { 'check' } else { 'build' }
$buildConfiguration = if ($Development) { 'debug' } else { 'release' }
$cargoArguments = @($buildAction, '--locked', '--target', $buildTarget)
if (-not $Development) { $cargoArguments += '--release' }
if ($Offline) { $cargoArguments += '--offline' }

Push-Location -LiteralPath $PSScriptRoot
try {
    if (-not (Test-Path -LiteralPath 'Cargo.lock' -PathType Leaf)) { throw 'Cargo.lock is missing.' }
    Write-Host ('Running: cargo ' + ($cargoArguments -join ' '))
    & $cargoCommand.Source @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo failed (exit $LASTEXITCODE)." }
    if ($Check) {
        Write-Host "Check completed: $buildTarget ($buildConfiguration); no executable produced."
    } else {
        Write-Host "Build completed: $buildTarget ($buildConfiguration). See README for output paths."
    }
} finally {
    Pop-Location
}
