#Requires -Version 7.0
<#
.SYNOPSIS
Build an unsigned, per-user Windows Velopack package. Run on Windows with
PowerShell 7, Rust's MSVC toolchain, the C++ build tools, and .NET SDK 8+.
.EXAMPLE
pwsh -NoProfile -File scripts/package-windows.ps1 -Version 0.1.0-pilot.1
.EXAMPLE
pwsh -NoProfile -File scripts/package-windows.ps1 -Version 0.1.0 -Channel stable -RuntimeDllDirectory C:\GeodeRuntime
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $Version,
    [ValidateSet('pilot', 'stable')][string] $Channel = 'pilot',
    [string] $RuntimeDllDirectory,
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'package-windows-lib.ps1')

if (-not $IsWindows) { throw 'Windows packaging must run on Windows.' }
Assert-NoVelopackEnvironment
$parsedVersion = $null
if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$' -or
    -not [semver]::TryParse($Version, [ref] $parsedVersion)) {
    throw 'Version must be a SemVer release version, for example 0.1.0-pilot.1.'
}

$repository = Split-Path -Parent $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'
# An explicit target-dir makes artifact lookup independent of user Cargo config.
$targetDirectory = Join-Path $repository 'target'
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $targetDirectory "packaging/$Channel/$Version"
}
$OutputDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) {
    throw "Output directory already exists; choose a new -OutputDirectory: $OutputDirectory"
}
if ($RuntimeDllDirectory) {
    $RuntimeDllDirectory = (Get-Item -LiteralPath $RuntimeDllDirectory -ErrorAction Stop).FullName
}

$previousGitHash = $env:GEODE_GIT_HASH
Push-Location $repository
try {
    $commit = (Invoke-PackagingTool git @('rev-parse', 'HEAD') | Out-String).Trim()
    $dirty = -not [string]::IsNullOrWhiteSpace((Invoke-PackagingTool git @('status', '--porcelain') | Out-String))
    $env:GEODE_GIT_HASH = $commit
    Invoke-PackagingTool dotnet @('tool', 'restore', '--tool-manifest', (Join-Path $repository '.config/dotnet-tools.json')) | Out-Host
    Invoke-PackagingTool cargo @(
        'build', '--release', '--locked', '--target', $target, '--target-dir', $targetDirectory,
        '-p', 'geode-app', '-p', 'geode-collector', '--bins'
    ) | Out-Host

    $payload = Join-Path $OutputDirectory 'payload'
    $symbols = Join-Path $OutputDirectory 'symbols'
    New-GeodePayload -BuildDirectory (Join-Path $targetDirectory "$target/release") `
        -PayloadDirectory $payload -SymbolsDirectory $symbols -RuntimeDllDirectory $RuntimeDllDirectory
    Write-GeodeReleaseManifest -PayloadDirectory $payload -Version $Version -Commit $commit -Dirty $dirty -Channel $Channel
    Copy-Item -LiteralPath (Join-Path $payload 'release.json') -Destination (Join-Path $OutputDirectory 'release.json')
    Compress-Archive -Path (Join-Path $symbols '*') -DestinationPath (Join-Path $OutputDirectory 'symbols.zip')

    # Setup.exe installs per-user. Do not bootstrap machine-wide prerequisites.
    # --skip-updates disables the pinned vpk tool's own version check only.
    Invoke-PackagingTool dotnet @(
        'tool', 'run', 'vpk', '--', 'pack', '--packId', 'Geode.Desktop', '--packTitle', 'Geode',
        '--packVersion', $Version, '--packDir', $payload, '--mainExe', 'geode.exe',
        '--runtime', 'win-x64', '--channel', $Channel, '--outputDir', (Join-Path $OutputDirectory 'releases'),
        '--skip-updates'
    ) | Out-Host
    Write-Host "Unsigned development package: $OutputDirectory"
}
finally {
    $env:GEODE_GIT_HASH = $previousGitHash
    Pop-Location
}
