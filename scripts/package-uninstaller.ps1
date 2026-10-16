param(
    [string]$Prefix = (Join-Path $HOME ".magician"),
    [string]$DataDir = (Join-Path $HOME "MagicianNotes"),
    [string]$TaskName = "Magican Backend",
    [switch]$Force,
    [switch]$DeleteData,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
function Invoke-Removal([string]$Description, [scriptblock]$Action) {
    if ($DryRun) {
        Write-Host "  would $Description"
    } else {
        & $Action
        Write-Host "  OK $Description"
    }
}

$supervisor = Join-Path $Prefix "magic-supervisor.exe"
if (Test-Path -LiteralPath $supervisor -PathType Leaf) {
    if (-not $DryRun) {
        try { & $supervisor client shutdown *> $null } catch { }
        Start-Sleep -Milliseconds 500
    }
}
$schtasks = "$env:SystemRoot\System32\schtasks.exe"
$taskExists = $false
try {
    & $schtasks /Query /TN $TaskName *> $null
    $taskExists = $LASTEXITCODE -eq 0
} catch { }
if ($taskExists) {
    if (-not $DryRun) {
        try { & $schtasks /End /TN $TaskName *> $null } catch { }
    }
    Invoke-Removal "remove the per-user Magican backend task" {
        & $schtasks /Delete /TN $TaskName /F *> $null
        if ($LASTEXITCODE -ne 0) { throw "Task Scheduler could not remove '$TaskName'." }
    }
}

if (Test-Path -LiteralPath $Prefix -PathType Container) {
    $remove = $Force
    if (-not $remove) {
        $remove = (Read-Host "Remove the installation at $Prefix? [y/N]") -match '^(?i:y|yes)$'
    }
    if ($remove) {
        Invoke-Removal "remove $Prefix" { Remove-Item -LiteralPath $Prefix -Recurse -Force }
    }
}

if ($DeleteData -and (Test-Path -LiteralPath $DataDir -PathType Container)) {
    if ($DryRun) {
        Write-Host "  would ask for the exact data path before removing $DataDir"
    } else {
        $typed = Read-Host "Type the full path '$DataDir' to remove notes, secrets, and memory"
        if ($typed -eq $DataDir) {
            Remove-Item -LiteralPath $DataDir -Recurse -Force
            Write-Host "  OK data root removed"
        } else {
            Write-Host "  Data root kept."
        }
    }
} elseif (Test-Path -LiteralPath $DataDir -PathType Container) {
    Write-Host "  Data root kept at $DataDir."
}

# Ignored best-effort supervisor/schtasks calls can leave a non-zero native
# exit code even though every requested removal completed successfully.
$global:LASTEXITCODE = 0
