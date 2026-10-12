# Shared by the Windows packaging entry point and its filesystem tests.
Set-StrictMode -Version Latest

function Assert-NoVelopackEnvironment {
    # vpk accepts CLI options through environment variables, including runtime
    # bootstrapping and signing. This script exclusively owns those options.
    $options = @(Get-ChildItem Env: | Where-Object { $_.Name -like 'VPK_*' } | Select-Object -ExpandProperty Name)
    if ($options.Count -gt 0) {
        # Values may contain signing credentials; report names only.
        throw "Unset inherited Velopack options before packaging: $($options -join ', ')."
    }
}

function Invoke-PackagingTool {
    param(
        [Parameter(Mandatory)][string] $FilePath,
        [Parameter(Mandatory)][string[]] $Arguments
    )

    # PowerShell normally continues after a failed native command. Every build
    # and packaging step must stop before using stale or incomplete artifacts.
    $PSNativeCommandUseErrorActionPreference = $false
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE."
    }
}

function New-GeodePayload {
    param(
        [Parameter(Mandatory)][string] $BuildDirectory,
        [Parameter(Mandatory)][string] $PayloadDirectory,
        [Parameter(Mandatory)][string] $SymbolsDirectory,
        [string] $RuntimeDllDirectory
    )

    $binaries = @('geode.exe', 'geode-collector.exe')
    $payloadFiles = @($binaries | ForEach-Object {
        $file = Get-Item -LiteralPath (Join-Path $BuildDirectory $_) -ErrorAction Stop
        if ($file.PSIsContainer) { throw "Expected an executable file: $($file.FullName)" }
        $file
    })
    $symbolFiles = @('geode.pdb', 'geode_collector.pdb' | ForEach-Object {
        # Rust replaces a target's hyphens with underscores in its MSVC PDB.
        $file = Get-Item -LiteralPath (Join-Path $BuildDirectory $_) -ErrorAction Stop
        if ($file.PSIsContainer) { throw "Expected a symbol file: $($file.FullName)" }
        $file
    })

    if ($RuntimeDllDirectory) {
        $runtimeDirectory = Get-Item -LiteralPath $RuntimeDllDirectory -ErrorAction Stop
        if (-not $runtimeDirectory.PSIsContainer) { throw 'RuntimeDllDirectory must be a directory.' }
        $runtimeFiles = @(Get-ChildItem -LiteralPath $runtimeDirectory.FullName -Force)
        # This first version accepts application-local DLLs only. Reject
        # installer paths and app files rather than silently overriding them.
        $reserved = @(
            'geode.exe', 'geode-collector.exe', 'release.json', 'Update.exe',
            'Squirrel.exe', 'Geode_ExecutionStub.exe', 'sq.version', 'current', 'packages'
        )
        foreach ($file in $runtimeFiles) {
            if ($file.Name -in $reserved) { throw "Reserved runtime filename: $($file.Name)" }
            if ($file.PSIsContainer) { throw "RuntimeDllDirectory must be flat: $($file.Name)" }
            if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Runtime files must not be links: $($file.Name)"
            }
        }
        $dlls = @($runtimeFiles | Where-Object { $_.Extension -ieq '.dll' })
        if ($dlls.Count -eq 0) { throw 'RuntimeDllDirectory contains no DLLs.' }
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($file in $dlls) {
            if (-not $names.Add($file.Name)) { throw "Duplicate runtime DLL filename: $($file.Name)" }
        }
        $payloadFiles += $dlls
        $symbolFiles += @($runtimeFiles | Where-Object {
            $_.Extension -ieq '.pdb' -and "$($_.BaseName).dll" -in $dlls.Name
        })
    }

    $symbolNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $symbolFiles) {
        if (-not $symbolNames.Add($file.Name)) { throw "Duplicate symbol filename: $($file.Name)" }
    }
    foreach ($directory in @($PayloadDirectory, $SymbolsDirectory)) {
        if (Test-Path -LiteralPath $directory) { throw "Staging directory already exists: $directory" }
        New-Item -ItemType Directory -Path $directory -ErrorAction Stop | Out-Null
    }
    foreach ($file in $payloadFiles) {
        Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $PayloadDirectory $file.Name) -ErrorAction Stop
    }
    foreach ($file in $symbolFiles) {
        Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $SymbolsDirectory $file.Name) -ErrorAction Stop
    }
}

function Write-GeodeReleaseManifest {
    param(
        [Parameter(Mandatory)][string] $PayloadDirectory,
        [Parameter(Mandatory)][string] $Version,
        [Parameter(Mandatory)][string] $Commit,
        [Parameter(Mandatory)][bool] $Dirty,
        [Parameter(Mandatory)][string] $Channel
    )

    $files = @(Get-ChildItem -LiteralPath $PayloadDirectory -File | Sort-Object Name | ForEach-Object {
        [ordered]@{
            name = $_.Name
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            size = $_.Length
            file_version = $_.VersionInfo.FileVersion
            product_version = $_.VersionInfo.ProductVersion
        }
    })
    [ordered]@{
        schema_version = 1
        package_id = 'Geode.Desktop'
        version = $Version
        commit = $Commit
        source_dirty = $Dirty
        channel = $Channel
        target = 'x86_64-pc-windows-msvc'
        files = $files
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $PayloadDirectory 'release.json') -Encoding utf8NoBOM
}
