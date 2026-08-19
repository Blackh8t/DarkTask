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

# Fresh/corporate Windows often needs TLS 1.2 enabled for Invoke-RestMethod.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

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

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Value
    )
    $dir = Split-Path -Parent $Path
    if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($Path, $Value, $utf8)
}

function Stop-DarkTaskAgent {
    Disable-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Out-Null

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

    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        $procs = Get-Process -Name "remote-agent" -ErrorAction SilentlyContinue
        if (-not $procs) { break }
        foreach ($proc in $procs) {
            Write-Host "Stopping remote-agent pid $($proc.Id) ..."
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 750
    }

    if (Get-Process -Name "remote-agent" -ErrorAction SilentlyContinue) {
        throw "remote-agent.exe is still running. Reboot or end remaining processes in Task Manager, then rerun install.ps1."
    }
}

function Get-AgentArch {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { "x86_64" }
        "ARM64" { "aarch64" }
        default { "x86_64" }
    }
}

function Register-DeviceIdentity {
    param(
        [Parameter(Mandatory = $true)][string]$IdentityPath,
        [Parameter(Mandatory = $true)][string]$AgentVersion
    )

    Write-Host "Enrolling device with $Base ..."
    $body = @{
        enrollment_token = $EnrollToken
        hostname         = $env:COMPUTERNAME
        platform         = "windows"
        arch             = (Get-AgentArch)
        agent_version    = $AgentVersion
    } | ConvertTo-Json -Compress

    $resp = Invoke-RestMethod `
        -Uri "$Base/api/v1/enroll" `
        -Method Post `
        -Body $body `
        -ContentType "application/json; charset=utf-8"

    $identityJson = (@{
        device_id    = [string]$resp.device_id
        device_token = [string]$resp.device_token
    } | ConvertTo-Json -Compress)
    Write-Utf8NoBom -Path $IdentityPath -Value $identityJson

    Write-Host "Enrolled device $($resp.device_id)"
}

function Publish-AgentBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        try {
            Copy-Item -Path $Source -Destination $Destination -Force -ErrorAction Stop
            return
        } catch {
            Write-Host "Binary locked, stopping agents and retrying ..."
            Stop-DarkTaskAgent
        }
    }
    throw "Could not replace $Destination."
}

function Test-AgentBinary {
    param([Parameter(Mandatory = $true)][string]$Binary)

    Unblock-File -Path $Binary -ErrorAction SilentlyContinue
    $outFile = Join-Path $env:TEMP "darktask-agent-status.txt"
    Remove-Item $outFile -Force -ErrorAction SilentlyContinue

    $proc = Start-Process -FilePath "cmd.exe" `
        -ArgumentList "/c", "`"$Binary`" status > `"$outFile`" 2>&1" `
        -Wait -PassThru -NoNewWindow

    if ($proc.ExitCode -eq -1073741515 -or $proc.ExitCode -eq 3221225781) {
        throw @"
remote-agent.exe cannot start on this PC (missing VC++ runtime DLL).
Install Microsoft Visual C++ Redistributable (x64), then rerun install.ps1:
  https://aka.ms/vs/17/release/vc_redist.x64.exe
"@
    }

    if ($proc.ExitCode -ne 0) {
        $details = Get-Content $outFile -ErrorAction SilentlyContinue
        throw "remote-agent.exe preflight failed (exit $($proc.ExitCode)).`n$details"
    }
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null
icacls.exe $ProgramDataDir /grant "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" 2>$null | Out-Null

Stop-DarkTaskAgent

if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    Write-Host "Removing existing $ServiceName service registration ..."
    sc.exe delete $ServiceName 2>$null | Out-Null
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if (-not (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) { break }
        Stop-DarkTaskAgent
    }
}

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

Publish-AgentBinary -Source $tempExe -Destination $Exe
Unblock-File -Path $Exe -ErrorAction SilentlyContinue
Remove-Item $tempExe -Force -ErrorAction SilentlyContinue
Set-Content -Path $VersionFile -Value $latest.version -Encoding ASCII -NoNewline

# Maintenance script: prefer sibling file (dev), else download from server.
$bundledMaint = Join-Path $PSScriptRoot "agent-maintenance.ps1"
if (Test-Path $bundledMaint) {
    Copy-Item -Path $bundledMaint -Destination $MaintenanceScript -Force
} else {
    Invoke-WebRequest -Uri "$Base/api/v1/agent/maintenance.ps1" -OutFile $MaintenanceScript -UseBasicParsing
}

$configJson = (@{
    server  = $Server
    enroll  = $EnrollToken
    max_fps = $MaxFps
} | ConvertTo-Json -Compress)
Write-Utf8NoBom -Path $Config -Value $configJson

# Full install from the portal always re-enrolls with the bundled token.
$Identity = Join-Path $ProgramDataDir "identity.json"
if (Test-Path $Identity) {
    Write-Host "Removing saved device identity for re-enroll ..."
    Remove-Item -Path $Identity -Force
}

Register-DeviceIdentity -IdentityPath $Identity -AgentVersion $latest.version

Write-Host "Preflight agent binary ..."
Test-AgentBinary -Binary $Exe

Stop-DarkTaskAgent

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

Write-Host "Starting $ServiceName ..."
sc.exe start $ServiceName | Out-Null
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
    $svc = Get-Service -Name $ServiceName
    if ($svc.Status -eq "Running") { break }
    Start-Sleep -Seconds 1
}
if ((Get-Service -Name $ServiceName).Status -ne "Running") {
    throw "Service did not reach Running within 30s. Check: Get-WinEvent -LogName System -MaxEvents 20 | Where Message -Match DarkTask"
}

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

    Enable-ScheduledTask -TaskName $TaskName | Out-Null
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
