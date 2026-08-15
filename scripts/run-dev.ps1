$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:REMOTE_ENROLL_TOKEN = "dev-enroll"
$env:RUST_LOG = "remote_server=info"
Write-Host "Starting server on http://127.0.0.1:8787"
cargo run -p remote-server
