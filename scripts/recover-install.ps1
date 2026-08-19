# Recover a PC after the old portal install.ps1 failed at Start-Service.
# Run elevated: powershell -ExecutionPolicy Bypass -File recover-install.ps1

param(
    [string]$Server = "https://portal.darktask.online",
    [string]$EnrollToken = ""
)

$ErrorActionPreference = "Stop"

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$InstallDir = "C:\Program Files\DarkTask"
$ProgramDataDir = "C:\ProgramData\DarkTask"
$Exe = Join-Path $InstallDir "remote-agent.exe"
$Config = Join-Path $ProgramDataDir "agent-config.json"
$Identity = Join-Path $ProgramDataDir "identity.json"
$ServiceName = "DarkTaskAgent"
$Base = $Server.TrimEnd("/")

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Value
    )
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($Path, $Value, $utf8)
}

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Run from an elevated PowerShell window."
}

if (-not (Test-Path $Exe)) {
    throw "Missing $Exe — rerun portal install or copy remote-agent.exe there first."
}

Write-Host "=== Reset service + processes ==="
& "$PSScriptRoot\force-reset-agent.ps1"

Write-Host "=== Fix ProgramData ACL ==="
New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null
icacls.exe $ProgramDataDir /reset /T /C 2>$null | Out-Null
icacls.exe $ProgramDataDir /grant "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" 2>$null | Out-Null

if (-not (Test-Path $Config)) {
    if (-not $EnrollToken) {
        throw "Missing $Config and no -EnrollToken supplied."
    }
    $cfg = @{
        server  = $Server
        enroll  = $EnrollToken
        max_fps           = 12
        capture_max_width = 800
        h264_bitrate      = 1000000
    }
} else {
    $cfg = Get-Content $Config -Raw | ConvertFrom-Json
    if ($EnrollToken) { $cfg.enroll = $EnrollToken }
}

Write-Utf8NoBom -Path $Config -Value (($cfg | ConvertTo-Json -Compress))

if (-not (Test-Path $Identity)) {
    Write-Host "=== Enroll device ==="
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq "AMD64") { "x86_64" } else { "x86_64" }
    $body = (@{
        enrollment_token = [string]$cfg.enroll
        hostname         = $env:COMPUTERNAME
        platform         = "windows"
        arch             = $arch
        agent_version    = "0.3.1"
    } | ConvertTo-Json -Compress)
    $resp = Invoke-RestMethod -Uri "$Base/api/v1/enroll" -Method Post -Body $body -ContentType "application/json; charset=utf-8"
    Write-Utf8NoBom -Path $Identity -Value ((@{
        device_id    = [string]$resp.device_id
        device_token = [string]$resp.device_token
    } | ConvertTo-Json -Compress))
    Write-Host "Enrolled $($resp.device_id)"
} else {
    Write-Host "=== Keeping existing identity ==="
}

Write-Host "=== Create service ==="
New-Service `
    -Name $ServiceName `
    -BinaryPathName "`"$Exe`" service" `
    -DisplayName "DarkTask Remote Agent" `
    -StartupType Automatic `
    -Description "DarkTask managed remote access agent" | Out-Null

Write-Host "=== Start service ==="
sc.exe start $ServiceName | Out-Null
Start-Sleep -Seconds 5
$svc = Get-Service $ServiceName
Write-Host "Service status: $($svc.Status)"
& $Exe status

if ($svc.Status -ne "Running") {
    throw "Service still not running. Reboot, then rerun recover-install.ps1."
}

Write-Host "Done — check portal for $($env:COMPUTERNAME)."
