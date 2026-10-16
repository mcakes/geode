#Requires -Version 7.0
# Dependency-free filesystem and native-process tests; also run on macOS/Linux.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'package-windows-lib.ps1')

function Assert-Equal($Actual, $Expected) {
    if ($Actual -cne $Expected) { throw "Expected '$Expected', got '$Actual'." }
}

function Assert-Fails([scriptblock] $Action, [string] $Message) {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_ }
    if ($null -eq $failure -or $failure.Exception.Message -notlike "*$Message*") {
        throw "Expected failure containing '$Message'; got '$failure'."
    }
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) "geode-packaging-tests-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    $previousFramework = $env:VPK_FRAMEWORK
    try {
        $env:VPK_FRAMEWORK = 'vcredist143-x64'
        Assert-Fails { Assert-NoVelopackEnvironment } 'VPK_FRAMEWORK'
    }
    finally { $env:VPK_FRAMEWORK = $previousFramework }
    Write-Host 'PASS: inherited vpk options cannot introduce machine-wide runtime installation'

    $build = Join-Path $scratch 'build output'
    $runtime = Join-Path $scratch 'runtime input'
    New-Item -ItemType Directory -Path $build, $runtime | Out-Null
    foreach ($name in @('geode.exe', 'geode-collector.exe', 'geode.pdb', 'geode_collector.pdb', 'stale.dll', 'private.toml', 'local.duckdb')) {
        Set-Content -LiteralPath (Join-Path $build $name) -Value "build: $name"
    }
    foreach ($name in @('pricing.dll', 'solace.DLL', 'pricing.pdb', 'unrelated.pdb', 'private.toml', 'local.duckdb')) {
        Set-Content -LiteralPath (Join-Path $runtime $name) -Value "vendor: $name"
    }

    $payload = Join-Path $scratch 'payload'
    $symbols = Join-Path $scratch 'symbols'
    New-GeodePayload -BuildDirectory $build -PayloadDirectory $payload -SymbolsDirectory $symbols -RuntimeDllDirectory $runtime
    Assert-Equal ((Get-ChildItem -LiteralPath $payload).Name | Sort-Object | Join-String -Separator ',') 'geode-collector.exe,geode.exe,pricing.dll,solace.DLL'
    Assert-Equal ((Get-ChildItem -LiteralPath $symbols).Name | Sort-Object | Join-String -Separator ',') 'geode_collector.pdb,geode.pdb,pricing.pdb'
    Assert-Equal (Get-Content -LiteralPath (Join-Path $payload 'pricing.dll')) 'vendor: pricing.dll'

    Write-GeodeReleaseManifest -PayloadDirectory $payload -Version '0.2.0-pilot.1' -Commit ('a' * 40) -Dirty $false -Channel pilot
    $manifest = Get-Content -LiteralPath (Join-Path $payload 'release.json') -Raw | ConvertFrom-Json
    Assert-Equal $manifest.package_id 'Geode.Desktop'
    Assert-Equal $manifest.version '0.2.0-pilot.1'
    Assert-Equal $manifest.commit ('a' * 40)
    Assert-Equal $manifest.source_dirty $false
    Assert-Equal $manifest.channel 'pilot'
    Assert-Equal $manifest.target 'x86_64-pc-windows-msvc'
    Assert-Equal ($manifest.files.name -join ',') 'geode-collector.exe,geode.exe,pricing.dll,solace.DLL'
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        foreach ($file in $manifest.files) {
            $bytes = [IO.File]::ReadAllBytes((Join-Path $payload $file.name))
            $hash = [BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
            Assert-Equal $file.sha256 $hash
            Assert-Equal $file.size $bytes.Length
        }
    }
    finally { $sha256.Dispose() }
    if ((Get-Content -LiteralPath (Join-Path $payload 'release.json') -Raw).Contains($scratch)) {
        throw 'The release manifest leaked a machine-local path.'
    }
    Write-Host 'PASS: payload allowlist, symbol separation, and release manifest hashes'

    $withoutVendor = Join-Path $scratch 'without-vendor'
    New-GeodePayload -BuildDirectory $build -PayloadDirectory $withoutVendor -SymbolsDirectory (Join-Path $scratch 'without-vendor-symbols')
    Assert-Equal ((Get-ChildItem -LiteralPath $withoutVendor).Name | Sort-Object | Join-String -Separator ',') 'geode-collector.exe,geode.exe'
    Write-Host 'PASS: optional vendor input never falls back to unrelated build DLLs'

    foreach ($reserved in @('GEODE.EXE', 'geode-collector.exe', 'UPDATE.EXE', 'Squirrel.exe', 'Geode_ExecutionStub.exe', 'release.json', 'sq.version', 'current', 'packages')) {
        $collision = Join-Path $runtime $reserved
        Set-Content -LiteralPath $collision -Value 'must not replace owned files'
        Assert-Fails {
            New-GeodePayload -BuildDirectory $build -PayloadDirectory (Join-Path $scratch 'collision-payload') `
                -SymbolsDirectory (Join-Path $scratch 'collision-symbols') -RuntimeDllDirectory $runtime
        } 'Reserved runtime filename'
        Assert-Equal (Test-Path -LiteralPath (Join-Path $scratch 'collision-payload')) $false
        Remove-Item -LiteralPath $collision
    }
    Write-Host 'PASS: case-insensitive app and updater filename collisions are rejected before staging'

    $collidingSymbols = Join-Path $scratch 'colliding-symbols'
    New-Item -ItemType Directory -Path $collidingSymbols | Out-Null
    Set-Content -LiteralPath (Join-Path $collidingSymbols 'geode.dll') -Value 'vendor DLL'
    Set-Content -LiteralPath (Join-Path $collidingSymbols 'GEODE.PDB') -Value 'vendor symbols'
    Assert-Fails {
        New-GeodePayload -BuildDirectory $build -PayloadDirectory (Join-Path $scratch 'symbol-collision-payload') `
            -SymbolsDirectory (Join-Path $scratch 'symbol-collision-symbols') -RuntimeDllDirectory $collidingSymbols
    } 'Duplicate symbol filename'
    Write-Host 'PASS: vendor symbols cannot overwrite application symbols'

    $emptyRuntime = Join-Path $scratch 'empty runtime'
    New-Item -ItemType Directory -Path $emptyRuntime | Out-Null
    Assert-Fails {
        New-GeodePayload -BuildDirectory $build -PayloadDirectory (Join-Path $scratch 'empty-payload') `
            -SymbolsDirectory (Join-Path $scratch 'empty-symbols') -RuntimeDllDirectory $emptyRuntime
    } 'contains no DLLs'
    $nested = Join-Path $runtime 'nested'
    New-Item -ItemType Directory -Path $nested | Out-Null
    Assert-Fails {
        New-GeodePayload -BuildDirectory $build -PayloadDirectory (Join-Path $scratch 'nested-payload') `
            -SymbolsDirectory (Join-Path $scratch 'nested-symbols') -RuntimeDllDirectory $runtime
    } 'must be flat'
    Remove-Item -LiteralPath $nested
    Assert-Fails {
        New-GeodePayload -BuildDirectory $build -PayloadDirectory (Join-Path $scratch 'missing-payload') `
            -SymbolsDirectory (Join-Path $scratch 'missing-symbols') -RuntimeDllDirectory (Join-Path $scratch 'missing')
    } 'does not exist'
    Write-Host 'PASS: requested missing, empty, or nested DLL inputs cannot silently produce an incomplete package'

    Set-Content -LiteralPath (Join-Path $payload 'keep.txt') -Value 'existing output'
    Assert-Fails {
        New-GeodePayload -BuildDirectory $build -PayloadDirectory $payload -SymbolsDirectory (Join-Path $scratch 'new-symbols')
    } 'already exists'
    Assert-Equal (Get-Content -LiteralPath (Join-Path $payload 'keep.txt')) 'existing output'
    Write-Host 'PASS: existing staging data is preserved'

    $pwsh = (Get-Process -Id $PID).Path
    $result = Invoke-PackagingTool -FilePath $pwsh -Arguments @('-NoLogo', '-NoProfile', '-Command', 'Write-Output success; exit 0')
    Assert-Equal $result 'success'
    Assert-Fails {
        Invoke-PackagingTool -FilePath $pwsh -Arguments @('-NoLogo', '-NoProfile', '-Command', 'exit 23')
        Set-Content -LiteralPath (Join-Path $scratch 'continued-after-failure') -Value 'wrong'
    } 'exit code 23'
    Assert-Equal (Test-Path -LiteralPath (Join-Path $scratch 'continued-after-failure')) $false
    Write-Host 'PASS: native tool failures stop subsequent packaging work'
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force
}
