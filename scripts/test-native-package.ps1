param()

$ErrorActionPreference = "Stop"
$TestRoot = Join-Path $env:TEMP ("magician-native-package-" + [guid]::NewGuid().ToString("N"))
$PackageRoot = Join-Path $TestRoot "package"
$Prefix = Join-Path $TestRoot "installed"
$DataRoot = Join-Path $TestRoot "MagicianNotes"
$Sentinel = Join-Path $DataRoot "sentinel.txt"
$TaskName = "Magican Backend Fixture " + [guid]::NewGuid().ToString("N")

try {
    New-Item -ItemType Directory -Path (Join-Path $PackageRoot "scripts") -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $PackageRoot "share\seed") -Force | Out-Null
    New-Item -ItemType Directory -Path $DataRoot -Force | Out-Null
    Set-Content -LiteralPath $Sentinel -Value "keep" -Encoding ASCII

    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "package-installer.ps1") `
        -Destination (Join-Path $PackageRoot "install.ps1")
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "package-uninstaller.ps1") `
        -Destination (Join-Path $PackageRoot "uninstall.ps1")
    foreach ($binary in @("magician.exe", "magicutor.exe", "magic-supervisor.exe")) {
        Copy-Item -LiteralPath "$env:SystemRoot\System32\cmd.exe" `
            -Destination (Join-Path $PackageRoot $binary)
    }
    @"
name: magician
version: fixture
target: x86_64-pc-windows-msvc
signing: none
"@ | Set-Content -LiteralPath (Join-Path $PackageRoot "MANIFEST.yaml") -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $PackageRoot "tool-runtime-config.yaml") `
        -Value "version: 1" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $PackageRoot "scripts\run-supervisor.sh") `
        -Value "fixture" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $PackageRoot "share\seed\magician-config.yaml") `
        -Value "fixture: true" -Encoding ASCII

    Push-Location -LiteralPath $PackageRoot
    try {
        $checksumLines = Get-ChildItem -LiteralPath . -File -Recurse |
            Where-Object { $_.Name -ne "SHA256SUMS" } |
            Sort-Object FullName |
            ForEach-Object {
                # Hosted Windows runners can expose $env:TEMP through a long
                # path while Get-ChildItem returns its 8.3 alias. Derive the
                # name from the provider's current location instead of slicing
                # one spelling with the other spelling's length.
                $relative = (Resolve-Path -LiteralPath $_.FullName -Relative) `
                    -replace '^\.[\\/]', ''
                $relative = $relative.Replace('\', '/')
                $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                "$hash  ./$relative"
            }
    } finally {
        Pop-Location
    }
    $checksumLines | Set-Content -LiteralPath (Join-Path $PackageRoot "SHA256SUMS") -Encoding ASCII

    & (Join-Path $PackageRoot "install.ps1") -Prefix $Prefix -Force
    $required = @(
        "magician.exe", "magicutor.exe", "magic-supervisor.exe",
        "MANIFEST.yaml", "SHA256SUMS", "install.ps1", "uninstall.ps1"
    )
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $Prefix $name) -PathType Leaf)) {
            throw "Installed fixture is missing $name"
        }
    }
    foreach ($name in @("magician.exe", "magicutor.exe", "magic-supervisor.exe")) {
        $sourceHash = (Get-FileHash -LiteralPath (Join-Path $PackageRoot $name) -Algorithm SHA256).Hash
        $installedHash = (Get-FileHash -LiteralPath (Join-Path $Prefix $name) -Algorithm SHA256).Hash
        if ($sourceHash -ne $installedHash) { throw "Installed hash differs for $name" }
    }

    Add-Content -LiteralPath (Join-Path $PackageRoot "magicutor.exe") -Value "tampered"
    $tamperRejected = $false
    try {
        & (Join-Path $PackageRoot "install.ps1") -Prefix $Prefix -Force
    } catch {
        $tamperRejected = $_.Exception.Message -like "*Checksum mismatch*"
    }
    if (-not $tamperRejected) { throw "The Windows installer accepted a tampered executable" }

    & (Join-Path $Prefix "uninstall.ps1") `
        -Prefix $Prefix -DataDir $DataRoot -TaskName $TaskName -Force
    if (Test-Path -LiteralPath $Prefix) { throw "The Windows uninstaller kept the install prefix" }
    if ((Get-Content -LiteralPath $Sentinel -Raw).Trim() -ne "keep") {
        throw "The Windows uninstaller changed the runtime data root"
    }

    Write-Host "windows native package contract: ok"
} finally {
    Remove-Item -LiteralPath $TestRoot -Recurse -Force -ErrorAction SilentlyContinue
}

$global:LASTEXITCODE = 0
