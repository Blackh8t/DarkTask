$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
cargo run -p remote-controller -- --server http://127.0.0.1:8787 devices
