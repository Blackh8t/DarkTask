# DarkTask agent maintenance — ensure service persistence and silent updates.
# Runs hidden via scheduled task (SYSTEM). No UI output unless DARKTASK_MAINT_VERBOSE=1.

$ErrorActionPreference = "Stop"

$InstallDir = "C:\Program Files\DarkTask"
$ProgramDataDir = "C:\ProgramData\DarkTask"
$Exe = Join-Path $InstallDir "remote-agent.exe"
$Config = Join-Path $ProgramDataDir "agent-config.json"
$VersionFile = Join-Path $ProgramDataDir "agent-version.txt"
$LogFile = Join-Path $ProgramDataDir "update.log"
$ServiceName = "DarkTaskAgent"

function Write-Log([string]$Message) {
    $line = "{0:u} {1}" -f (Get-Date), $Message
    Add-Content -Path $LogFile -Value $line -Encoding UTF8
    if ($env:DARKTASK_MAINT_VERBOSE -eq "1") {
        Write-Host $line
    }
}

function Ensure-ServiceRunning {
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if (-not $svc) {
        Write-Log "Service $ServiceName is not installed."
        return
    }
    if ($svc.Status -ne "Running") {
        Write-Log "Starting $ServiceName (was $($svc.Status))."
        Start-Service -Name $ServiceName -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
    $svc.Refresh()
    if ($svc.StartType -ne "Automatic") {
        Write-Log "Setting $ServiceName start type to Automatic."
        Set-Service -Name $ServiceName -StartupType Automatic
    }
}

function Get-InstalledVersion {
    if (Test-Path $VersionFile) {
        return (Get-Content -Path $VersionFile -Raw).Trim()
    }
    if (Test-Path $Exe) {
        return (Get-Item $Exe).VersionInfo.ProductVersion
    }
    return ""
}

function Test-NewerVersion([string]$Remote, [string]$Local) {
    if ([string]::IsNullOrWhiteSpace($Remote)) { return $false }
    if ([string]::IsNullOrWhiteSpace($Local)) { return $true }
    try {
        return [version]$Remote -gt [version]$Local
    } catch {
        return ($Remote -ne $Local)
    }
}

function Install-AgentUpdate {
    param(
        [string]$Server,
        [string]$TempExe,
        [string]$ExpectedSha256,
        [string]$NewVersion
    )

    if (-not (Test-Path $TempExe)) {
        throw "Downloaded agent missing: $TempExe"
    }

    if ($ExpectedSha256) {
        $hash = (Get-FileHash -Path $TempExe -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne $ExpectedSha256.ToLowerInvariant()) {
            throw "SHA256 mismatch for downloaded agent (got $hash)."
        }
    }

    Write-Log "Updating agent to $NewVersion."
    Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2

    Copy-Item -Path $TempExe -Destination $Exe -Force
    Set-Content -Path $VersionFile -Value $NewVersion -Encoding UTF8 -NoNewline

    Start-Service -Name $ServiceName
    Write-Log "Agent updated to $NewVersion."
}

function Invoke-UpdateCheck {
    if (-not (Test-Path $Config)) {
        Write-Log "Config not found; skipping update check."
        return
    }

    $cfg = Get-Content -Path $Config -Raw | ConvertFrom-Json
    $server = [string]$cfg.server
    if ([string]::IsNullOrWhiteSpace($server)) {
        Write-Log "Server URL missing in config; skipping update check."
        return
    }

    $base = $server.TrimEnd("/")
    $latest = Invoke-RestMethod -Uri "$base/api/v1/agent/latest" -UseBasicParsing
    $localVersion = Get-InstalledVersion

    if (-not (Test-NewerVersion -Remote $latest.version -Local $localVersion)) {
        Write-Log "Agent up to date ($localVersion)."
        return
    }

    $downloadUrl = if ($latest.download_url -match "^https?://") {
        $latest.download_url
    } else {
        "$base$($latest.download_url)"
    }

    $tempExe = Join-Path $env:TEMP "darktask-remote-agent.exe"
    if (Test-Path $tempExe) { Remove-Item $tempExe -Force }

    Write-Log "Downloading agent $($latest.version) from $downloadUrl"
    Invoke-WebRequest -Uri $downloadUrl -OutFile $tempExe -UseBasicParsing

    Install-AgentUpdate -Server $base -TempExe $tempExe -ExpectedSha256 $latest.sha256 -NewVersion $latest.version
    Remove-Item $tempExe -Force -ErrorAction SilentlyContinue
}

try {
    Ensure-ServiceRunning
    Invoke-UpdateCheck
} catch {
    Write-Log "Maintenance error: $($_.Exception.Message)"
    exit 1
}
