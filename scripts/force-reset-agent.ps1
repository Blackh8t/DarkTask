# Force-stop DarkTask agent so install.ps1 can replace remote-agent.exe.
# Run elevated: powershell -ExecutionPolicy Bypass -File scripts\force-reset-agent.ps1

$ErrorActionPreference = "Stop"
$ServiceName = "DarkTaskAgent"
$TaskName = "DarkTask Agent Maintenance"

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Run from an elevated PowerShell window."
}

Write-Host "Disabling maintenance task ..."
Disable-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Out-Null

$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($svc) {
    Write-Host "Service status: $($svc.Status)"
    if ($svc.Status -ne "Stopped") {
        Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
    }
    sc.exe stop $ServiceName 2>$null | Out-Null
    Start-Sleep -Seconds 2
    Write-Host "Deleting service registration ..."
    sc.exe delete $ServiceName 2>$null | Out-Null
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if (-not (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) { break }
    }
}

$deadline = (Get-Date).AddSeconds(20)
while ((Get-Date) -lt $deadline) {
    $procs = Get-Process -Name "remote-agent" -ErrorAction SilentlyContinue
    if (-not $procs) { break }
    foreach ($proc in $procs) {
        Write-Host "Killing remote-agent pid $($proc.Id) ..."
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 750
}

if (Get-Process -Name "remote-agent" -ErrorAction SilentlyContinue) {
    throw "remote-agent.exe still running. Reboot, then rerun install.ps1."
}

Write-Host "DarkTask agent stopped. Safe to run install.ps1 now."
