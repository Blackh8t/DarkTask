param(
    [Parameter(Mandatory=$true)][string]$DeviceId,
    [string]$Server = "http://127.0.0.1:8787",
    [string]$AdminToken = $env:REMOTE_ADMIN_TOKEN
)
if (-not $AdminToken) { throw "Set REMOTE_ADMIN_TOKEN or pass -AdminToken" }
$env:REMOTE_ADMIN_TOKEN = $AdminToken
& "$PSScriptRoot\..\target\release\remote-controller.exe" --server $Server connect $DeviceId
