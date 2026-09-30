param(
    [string]$Prefix = (Join-Path $HOME ".magician"),
    [switch]$Force,
    [switch]$NoVerify
)

$ErrorActionPreference = "Stop"
$PackageRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

function Read-ManifestValue([string]$Name) {
    $line = Get-Content -LiteralPath (Join-Path $PackageRoot "MANIFEST.yaml") |
        Where-Object { $_ -match "^$([regex]::Escape($Name)):\s*(.+)$" } |
        Select-Object -First 1
    if (-not $line) { return "" }
    return ([regex]::Match($line, "^[^:]+:\s*(.+)$")).Groups[1].Value.Trim()
}

function Assert-PackageChecksums {
    $checksumPath = Join-Path $PackageRoot "SHA256SUMS"
    if (-not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
        throw "SHA256SUMS is missing; download the package again."
    }
    $root = [IO.Path]::GetFullPath($PackageRoot).TrimEnd('\') + '\'
    foreach ($line in Get-Content -LiteralPath $checksumPath) {
        if ($line -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
            throw "Malformed SHA256SUMS entry: $line"
        }
        $expected = $Matches[1].ToLowerInvariant()
        $relative = $Matches[2] -replace '^\.[\\/]', ''
        $candidate = [IO.Path]::GetFullPath((Join-Path $PackageRoot $relative))
        if (-not $candidate.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Checksum entry escapes the package root: $relative"
        }
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            throw "Package file is missing: $relative"
        }
        $actual = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "Checksum mismatch: $relative"
        }
    }
}

$required = @(
    "MANIFEST.yaml", "SHA256SUMS", "install.ps1", "tool-runtime-config.yaml",
    "magician.exe", "magicutor.exe", "magic-supervisor.exe"
)
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $PackageRoot $name) -PathType Leaf)) {
        throw "Native package is incomplete: $name is missing."
    }
}

$target = Read-ManifestValue "target"
if ($target -ne "x86_64-pc-windows-msvc") {
    throw "Package target '$target' is not supported by this Windows installer."
}

if (-not $NoVerify) {
    Assert-PackageChecksums
    Write-Host "  OK package checksums verify"
}

if ((Test-Path -LiteralPath $Prefix) -and (Get-ChildItem -LiteralPath $Prefix -Force -ErrorAction SilentlyContinue)) {
    if (-not $Force) {
        $answer = Read-Host "An installation already exists at $Prefix. Replace it? [y/N]"
        if ($answer -notmatch '^(?i:y|yes)$') {
            Write-Host "Nothing was changed."
            exit 0
        }
    }
}

$existingSupervisor = Join-Path $Prefix "magic-supervisor.exe"
if (Test-Path -LiteralPath $existingSupervisor -PathType Leaf) {
    try {
        & $existingSupervisor client shutdown *> $null
    } catch {
        # Updating a stopped or unhealthy install is still valid. The copy
        # below remains the authority on whether an executable is unlocked.
    }
    Start-Sleep -Milliseconds 500
}

New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
foreach ($directory in @("scripts", "share")) {
    $source = Join-Path $PackageRoot $directory
    $destination = Join-Path $Prefix $directory
    if (Test-Path -LiteralPath $source -PathType Container) {
        Remove-Item -LiteralPath $destination -Recurse -Force -ErrorAction SilentlyContinue
        Copy-Item -LiteralPath $source -Destination $destination -Recurse -Force
    }
}
foreach ($file in @("tool-runtime-config.yaml", "MANIFEST.yaml", "SHA256SUMS", "install.ps1", "uninstall.ps1")) {
    $source = Join-Path $PackageRoot $file
    if (Test-Path -LiteralPath $source -PathType Leaf) {
        Copy-Item -LiteralPath $source -Destination (Join-Path $Prefix $file) -Force
    }
}
foreach ($binary in @("magician.exe", "magicutor.exe", "magic-supervisor.exe")) {
    Copy-Item -LiteralPath (Join-Path $PackageRoot $binary) -Destination (Join-Path $Prefix $binary) -Force
}

Write-Host "  OK installed to $Prefix"
Write-Host "Open Magican Desktop to finish setup and register the per-user background task."
Write-Host "Your data at $HOME\MagicianNotes was not changed."
$global:LASTEXITCODE = 0
