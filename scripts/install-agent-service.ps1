param(
    [Parameter(Mandatory=$false)]
    [string]$Server = "http://62.72.31.30:8789",

    [Parameter(Mandatory=$true)]
    [string]$EnrollToken,

    [Parameter(Mandatory=$false)]
    [string]$SourceExe = ".\target\release\remote-agent.exe",

    [Parameter(Mandatory=$false)]
    [int]$MaxFps = 20,

    [Parameter(Mandatory=$false)]
    [string]$Desktop = "DarkTask-2"
)

$ErrorActionPreference = "Stop"

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Run this installer from an elevated PowerShell window."
}

$InstallDir = "C:\Program Files\DarkTask"
$ProgramDataDir = "C:\ProgramData\DarkTask"
$Exe = Join-Path $InstallDir "remote-agent.exe"
$Config = Join-Path $ProgramDataDir "agent-config.json"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null

if (-not (Test-Path $SourceExe)) {
    throw "Agent executable not found: $SourceExe"
}

Copy-Item $SourceExe $Exe -Force

$configObject = [ordered]@{
    server = $Server
    enroll = $EnrollToken
    max_fps = $MaxFps
    desktop = $Desktop
}

$configObject | ConvertTo-Json | Set-Content -Path $Config -Encoding UTF8

# Restrict config/identity directory to SYSTEM + Administrators.
icacls.exe $ProgramDataDir /inheritance:r | Out-Null
icacls.exe $ProgramDataDir /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" | Out-Null

sc.exe stop DarkTaskAgent 2>$null | Out-Null
sc.exe delete DarkTaskAgent 2>$null | Out-Null
Start-Sleep -Milliseconds 500

$binPath = "`"$Exe`" service"

sc.exe create DarkTaskAgent `
    binPath= $binPath `
    start= auto `
    obj= LocalSystem `
    DisplayName= "DarkTask Remote Agent"

if ($LASTEXITCODE -ne 0) {
    throw "Failed to create DarkTaskAgent service."
}

sc.exe description DarkTaskAgent "DarkTask managed remote access agent"
sc.exe failure DarkTaskAgent reset= 86400 actions= restart/5000/restart/5000/restart/5000
sc.exe failureflag DarkTaskAgent 1

sc.exe start DarkTaskAgent

Write-Host ""
Write-Host "DarkTaskAgent installed."
Write-Host "Service:"
sc.exe query DarkTaskAgent
Write-Host ""
Write-Host "Config: $Config"
Write-Host "Identity will be stored at C:\ProgramData\DarkTask\identity.json after first enrollment."
