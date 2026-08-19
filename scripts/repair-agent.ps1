# One-shot repair when install hangs on "Waiting for service to start".
# Run elevated: powershell -ExecutionPolicy Bypass -File scripts\repair-agent.ps1

param(
    [string]$Server = "https://portal.darktask.online",
    [string]$EnrollToken = "1cca80040f6cc9eeb7d2e2af561ce3b415c73e5db97c3a3af2a6bcf299827a6d"
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

Write-Host "=== Step 1: stop service and processes ==="
& "$PSScriptRoot\force-reset-agent.ps1"

Write-Host "=== Step 2: reset data folder ACL ==="
New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null
icacls.exe $ProgramDataDir /reset /T /C 2>$null | Out-Null

Write-Host "=== Step 3: write config + enroll (UTF-8, no BOM) ==="
$configJson = (@{
    server            = $Server
    enroll            = $EnrollToken
    max_fps           = 12
    capture_max_width = 800
    h264_bitrate      = 1000000
} | ConvertTo-Json -Compress)
Write-Utf8NoBom -Path $Config -Value $configJson

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "AMD64") { "x86_64" } else { "x86_64" }
$body = (@{
    enrollment_token = $EnrollToken
    hostname         = $env:COMPUTERNAME
    platform         = "windows"
    arch             = $arch
    agent_version    = "0.3.1"
} | ConvertTo-Json -Compress)

$resp = Invoke-RestMethod -Uri "$Base/api/v1/enroll" -Method Post -Body $body -ContentType "application/json; charset=utf-8"
$identityJson = (@{
    device_id    = $resp.device_id
    device_token = $resp.device_token
} | ConvertTo-Json -Compress)
Write-Utf8NoBom -Path $Identity -Value $identityJson
Write-Host "Enrolled: $($resp.device_id)"

Write-Host "=== Step 4: recreate service ==="
if (-not (Test-Path $Exe)) { throw "Missing $Exe - run install.ps1 first or copy remote-agent.exe there." }

sc.exe delete $ServiceName 2>$null | Out-Null
Start-Sleep -Seconds 2

New-Service `
    -Name $ServiceName `
    -BinaryPathName "`"$Exe`" service" `
    -DisplayName "DarkTask Remote Agent" `
    -StartupType Automatic `
    -Description "DarkTask managed remote access agent" | Out-Null

Write-Host "=== Step 5: start service ==="
sc.exe start $ServiceName | Out-Null
Start-Sleep -Seconds 5

$svc = Get-Service $ServiceName
Write-Host "Service status: $($svc.Status)"
& "$Exe" status

if ($svc.Status -ne "Running") {
    Write-Host "Waiting up to 30s ..."
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        $svc.Refresh()
        if ($svc.Status -eq "Running") { break }
        Start-Sleep -Seconds 1
    }
    Write-Host "Service status: $((Get-Service $ServiceName).Status)"
}

if ((Get-Service $ServiceName).Status -eq "Running") {
    Write-Host "Done - check portal for this PC."
} else {
    throw "Service still not running. Reboot, then rerun repair-agent.ps1."
}
