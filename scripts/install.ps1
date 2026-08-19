# DarkTask Windows agent installer
# Download from the admin portal (pre-configured) or run locally with -EnrollToken.
#
# Installs:
#   - remote-agent.exe Windows service (LocalSystem, auto-start)
#   - agent-maintenance.ps1 + scheduled task (boot + daily update check)
#
param(
    [Parameter(Mandatory = $false)]
    [string]$Server = "__DARKTASK_SERVER__",

    [Parameter(Mandatory = $false)]
    [string]$EnrollToken = "__DARKTASK_ENROLL__",

    [Parameter(Mandatory = $false)]
    [int]$MaxFps = 12,

    [Parameter(Mandatory = $false)]
    [switch]$SkipUpdateTask
)

$ErrorActionPreference = "Stop"

if ($EnrollToken -like "*DARKTASK_ENROLL*" -or $Server -like "*DARKTASK_SERVER*") {
    throw @"
Download install.ps1 from the DarkTask admin portal (Enroll device card).
It includes your server URL and enrollment token.
"@
}

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Run install.ps1 from an elevated PowerShell window."
}

$InstallDir = "C:\Program Files\DarkTask"
$ProgramDataDir = "C:\ProgramData\DarkTask"
$Exe = Join-Path $InstallDir "remote-agent.exe"
$MaintenanceScript = Join-Path $InstallDir "agent-maintenance.ps1"
$Config = Join-Path $ProgramDataDir "agent-config.json"
$VersionFile = Join-Path $ProgramDataDir "agent-version.txt"
$ServiceName = "DarkTaskAgent"
$TaskName = "DarkTask Agent Maintenance"
$Base = $Server.TrimEnd("/")

function Stop-DarkTaskAgent {
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne "Stopped") {
        Write-Host "Stopping $ServiceName ..."
        Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
        $deadline = (Get-Date).AddSeconds(15)
        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 500
            $svc.Refresh()
            if ($svc.Status -eq "Stopped") { break }
        }
    }

    sc.exe stop $ServiceName 2>$null | Out-Null
    Start-Sleep -Milliseconds 500

    # Worker sessions keep remote-agent.exe open after the service stops.
    Get-Process -Name "remote-agent" -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "Stopping worker pid $($_.Id) ..."
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null

Stop-DarkTaskAgent

Write-Host "Downloading latest agent from $Base ..."
$latest = Invoke-RestMethod -Uri "$Base/api/v1/agent/latest" -UseBasicParsing
$downloadUrl = if ($latest.download_url -match "^https?://") {
    $latest.download_url
} else {
    "$Base$($latest.download_url)"
}

$tempExe = Join-Path $env:TEMP "darktask-remote-agent-install.exe"
Invoke-WebRequest -Uri $downloadUrl -OutFile $tempExe -UseBasicParsing

if ($latest.sha256) {
    $hash = (Get-FileHash -Path $tempExe -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $latest.sha256.ToLowerInvariant()) {
        throw "Downloaded agent failed SHA256 verification."
    }
}

Copy-Item -Path $tempExe -Destination $Exe -Force
Remove-Item $tempExe -Force -ErrorAction SilentlyContinue
Set-Content -Path $VersionFile -Value $latest.version -Encoding UTF8 -NoNewline

# Maintenance script: prefer sibling file (dev), else download from server.
$bundledMaint = Join-Path $PSScriptRoot "agent-maintenance.ps1"
if (Test-Path $bundledMaint) {
    Copy-Item -Path $bundledMaint -Destination $MaintenanceScript -Force
} else {
    Invoke-WebRequest -Uri "$Base/api/v1/agent/maintenance.ps1" -OutFile $MaintenanceScript -UseBasicParsing
}

@{
    server  = $Server
    enroll  = $EnrollToken
    max_fps = $MaxFps
} | ConvertTo-Json | Set-Content -Path $Config -Encoding UTF8

# Full install from the portal always re-enrolls with the bundled token.
$Identity = Join-Path $ProgramDataDir "identity.json"
if (Test-Path $Identity) {
    Write-Host "Removing saved device identity for re-enroll ..."
    Remove-Item -Path $Identity -Force
}

icacls.exe $ProgramDataDir /inheritance:r | Out-Null
icacls.exe $ProgramDataDir /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" | Out-Null

if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    sc.exe delete $ServiceName 2>$null | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if (-not (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) { break }
    }
}

$serviceBin = "`"$Exe`" service"
New-Service `
    -Name $ServiceName `
    -BinaryPathName $serviceBin `
    -DisplayName "DarkTask Remote Agent" `
    -StartupType Automatic `
    -Description "DarkTask managed remote access agent" `
    -ErrorAction Stop | Out-Null

sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/5000/restart/5000 | Out-Null
sc.exe failureflag $ServiceName 1 | Out-Null
Start-Service -Name $ServiceName

if (-not $SkipUpdateTask) {
    $action = New-ScheduledTaskAction `
        -Execute "powershell.exe" `
        -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$MaintenanceScript`""

    $triggerBoot = New-ScheduledTaskTrigger -AtStartup
    $triggerDaily = New-ScheduledTaskTrigger -Daily -At "03:15"

    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -StartWhenAvailable `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 15) `
        -MultipleInstances IgnoreNew

    $taskPrincipal = New-ScheduledTaskPrincipal `
        -UserId "SYSTEM" `
        -LogonType ServiceAccount `
        -RunLevel Highest

    Register-ScheduledTask `
        -TaskName $TaskName `
        -Action $action `
        -Trigger @($triggerBoot, $triggerDaily) `
        -Settings $settings `
        -Principal $taskPrincipal `
        -Force | Out-Null

    Write-Host "Scheduled task registered: $TaskName (At startup + daily 03:15)."
}

Write-Host ""
Write-Host "DarkTask agent installed."
Write-Host "  Binary     : $Exe"
Write-Host "  Version    : $($latest.version)"
Write-Host "  Config     : $Config"
Write-Host "  Maintenance: $MaintenanceScript"
Write-Host "  Identity   : $ProgramDataDir\identity.json (created on first enroll)"
sc.exe query $ServiceName
