# Legacy wrapper — use install.ps1 instead.
param(
    [Parameter(Mandatory = $true)]
    [string]$EnrollToken,

    [Parameter(Mandatory = $false)]
    [string]$Server = "http://62.72.31.30:8789",

    [Parameter(Mandatory = $false)]
    [int]$MaxFps = 12
)

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
& (Join-Path $here "install.ps1") -Server $Server -EnrollToken $EnrollToken -MaxFps $MaxFps
