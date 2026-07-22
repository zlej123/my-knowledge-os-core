[CmdletBinding()]
param(
    [string]$RepoRoot,
    [string]$CodexHome,
    [switch]$PlanOnly,
    [switch]$Yes,
    [switch]$SkipSkill
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-ExistingDirectory([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Label directory does not exist: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $RepoRoot = Join-Path $PSScriptRoot "..\..\..\.."
}
$RepoRoot = Resolve-ExistingDirectory $RepoRoot "Repository"

$CargoManifest = Join-Path $RepoRoot "rust\mko-cli\Cargo.toml"
$SkillSource = Join-Path $RepoRoot "skills\codex\my-knowledge-os"
if (-not (Test-Path -LiteralPath $CargoManifest -PathType Leaf)) {
    throw "mko CLI source was not found at $CargoManifest. Clone zlej123/my-knowledge-os-core and pass -RepoRoot."
}
if (-not (Test-Path -LiteralPath (Join-Path $SkillSource "SKILL.md") -PathType Leaf)) {
    throw "Canonical My Knowledge OS skill was not found at $SkillSource."
}

if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
        $CodexHome = $env:CODEX_HOME
    } else {
        $CodexHome = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".codex"
    }
}
$CodexHome = [IO.Path]::GetFullPath($CodexHome)
$SkillTarget = Join-Path $CodexHome "skills\my-knowledge-os"

$CargoHome = if (-not [string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    [IO.Path]::GetFullPath($env:CARGO_HOME)
} else {
    Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo"
}
$MkoBinary = Join-Path $CargoHome "bin\mko.exe"
$CargoCommand = Get-Command cargo -ErrorAction SilentlyContinue

Write-Host "My Knowledge OS source installation plan"
Write-Host "  repository : $RepoRoot"
Write-Host "  CLI source : $(Split-Path $CargoManifest -Parent)"
Write-Host "  CLI target : $MkoBinary"
if ($SkipSkill) {
    Write-Host "  Codex skill: skipped"
} else {
    Write-Host "  Codex skill: $SkillTarget"
}
Write-Host "  setup      : not run"

if ($null -eq $CargoCommand) {
    Write-Host ""
    Write-Host "Rust/Cargo is missing. Install Rust 1.97 or newer from https://rustup.rs/, restart PowerShell, and rerun this command."
    exit 2
}

if ($PlanOnly) {
    exit 0
}

if (-not $Yes) {
    if (-not [Environment]::UserInteractive) {
        throw "Interactive confirmation is unavailable. Review the plan, then rerun with -Yes."
    }
    $Answer = Read-Host "Type INSTALL to build the CLI and install the Codex skill"
    if ($Answer -cne "INSTALL") {
        throw "Installation cancelled."
    }
}

$CargoExecutable = $CargoCommand.Source
& $CargoExecutable install --path (Split-Path $CargoManifest -Parent) --locked --force
if ($LASTEXITCODE -ne 0) {
    throw "cargo install failed with exit code $LASTEXITCODE."
}
if (-not (Test-Path -LiteralPath $MkoBinary -PathType Leaf)) {
    throw "cargo reported success but mko was not found at $MkoBinary."
}

if (-not $SkipSkill) {
    $SkillParent = Split-Path $SkillTarget -Parent
    New-Item -ItemType Directory -Force -Path $SkillParent | Out-Null
    $Nonce = [Guid]::NewGuid().ToString("N")
    $Stage = "$SkillTarget.stage-$Nonce"
    $Backup = "$SkillTarget.backup-$((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ'))-$Nonce"
    try {
        Copy-Item -LiteralPath $SkillSource -Destination $Stage -Recurse
        if (-not (Test-Path -LiteralPath (Join-Path $Stage "SKILL.md") -PathType Leaf)) {
            throw "Staged skill is missing SKILL.md."
        }
        if (Test-Path -LiteralPath $SkillTarget) {
            Move-Item -LiteralPath $SkillTarget -Destination $Backup
            Write-Host "  previous skill backup: $Backup"
        }
        Move-Item -LiteralPath $Stage -Destination $SkillTarget
    } catch {
        if ((Test-Path -LiteralPath $Backup) -and -not (Test-Path -LiteralPath $SkillTarget)) {
            Move-Item -LiteralPath $Backup -Destination $SkillTarget
        }
        throw
    } finally {
        if (Test-Path -LiteralPath $Stage) {
            Remove-Item -LiteralPath $Stage -Recurse -Force
        }
    }
}

$CargoBin = Split-Path $MkoBinary -Parent
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
$PathEntries = @($UserPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
$ExpandedPathEntries = @($PathEntries | ForEach-Object {
    [Environment]::ExpandEnvironmentVariables($_).TrimEnd("\")
})
if (-not ($ExpandedPathEntries -contains $CargoBin.TrimEnd("\"))) {
    $NewUserPath = if ([string]::IsNullOrWhiteSpace($UserPath)) { $CargoBin } else { "$UserPath;$CargoBin" }
    [Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
    $env:Path = "$env:Path;$CargoBin"
    Write-Host "  PATH       : added $CargoBin to the user PATH"
}

$Version = & $MkoBinary --version
if ($LASTEXITCODE -ne 0) {
    throw "Installed mko failed its version check."
}

Write-Host ""
Write-Host "Installed: $Version"
Write-Host "Restart Codex so it reloads the skill. Then say: My Knowledge OS 시작해줘"
Write-Host "The installer intentionally did not run mko setup or mutate a knowledge repository."
