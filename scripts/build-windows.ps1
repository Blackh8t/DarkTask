$ErrorActionPreference = 'Stop'
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo build --release -p remote-agent -p remote-controller
    Write-Host ""
    Write-Host "Built:" -ForegroundColor Green
    Write-Host "  target\release\remote-agent.exe"
    Write-Host "  target\release\remote-controller.exe"
} finally {
    Pop-Location
}
