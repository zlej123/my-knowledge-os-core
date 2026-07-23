[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^mko-setup-plan-[0-9a-f]{64}$')]
    [string]$PlanId,

    [string]$MkoPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($MkoPath)) {
    $MkoCommand = Get-Command mko -ErrorAction SilentlyContinue
    if ($null -eq $MkoCommand) {
        throw "mko was not found on PATH. Pass its absolute path with -MkoPath."
    }
    $MkoPath = $MkoCommand.Source
}

$MkoPath = [IO.Path]::GetFullPath($MkoPath)
if (-not (Test-Path -LiteralPath $MkoPath -PathType Leaf)) {
    throw "mko executable was not found at $MkoPath."
}

$EscapedMkoPath = $MkoPath.Replace("'", "''")
$Command = "& '$EscapedMkoPath' setup apply --plan '$PlanId' --format json-v2"
$EncodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Command))
$Arguments = @("-NoExit", "-NoProfile", "-EncodedCommand", $EncodedCommand)

if ($PSCmdlet.ShouldProcess("a visible PowerShell window", "open the setup approval prompt")) {
    Start-Process -FilePath "powershell.exe" -ArgumentList $Arguments -WindowStyle Normal
    Write-Host "Opened the setup approval prompt. Review the paths and type the exact phrase in that window."
}
