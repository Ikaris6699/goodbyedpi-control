[CmdletBinding()]
param(
    [switch]$SkipRuntimeDownload
)

$ErrorActionPreference = 'Stop'
$GuiRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$RuntimeRoot = Join-Path $GuiRoot 'assets\runtime'
$TargetRoot = Join-Path $GuiRoot 'target\release'

New-Item -ItemType Directory -Force -Path $RuntimeRoot | Out-Null

if (-not $SkipRuntimeDownload) {
    Write-Host 'Downloading the latest official GoodbyeDPI release...'
    $headers = @{ Accept = 'application/vnd.github+json'; 'User-Agent' = 'GoodbyeDPI-Control-build' }
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/ValdikSS/GoodbyeDPI/releases/latest' -Headers $headers
    $asset = $release.assets | Where-Object { $_.name -match '\.zip$' } | Select-Object -First 1
    if ($null -eq $asset) {
        throw 'No .zip release asset was found on the latest GoodbyeDPI release.'
    }

    $download = Join-Path $env:TEMP $asset.name
    Invoke-WebRequest -Uri $asset.browser_download_url -Headers @{ 'User-Agent' = 'GoodbyeDPI-Control-build' } -OutFile $download
    $extract = Join-Path $env:TEMP ('goodbyedpi-release-' + [guid]::NewGuid().ToString('N'))
    Expand-Archive -LiteralPath $download -DestinationPath $extract -Force

    $backend = Get-ChildItem -LiteralPath $extract -Recurse -Filter 'goodbyedpi.exe' | Select-Object -First 1
    $divertDll = Get-ChildItem -LiteralPath $extract -Recurse -Filter 'WinDivert.dll' | Select-Object -First 1
    $divertSys = Get-ChildItem -LiteralPath $extract -Recurse -Filter 'WinDivert64.sys' | Select-Object -First 1
    if ($null -eq $backend -or $null -eq $divertDll -or $null -eq $divertSys) {
        throw 'The release did not contain goodbyedpi.exe, WinDivert.dll and WinDivert64.sys.'
    }

    Copy-Item -LiteralPath $backend.FullName -Destination (Join-Path $RuntimeRoot 'goodbyedpi.exe') -Force
    Copy-Item -LiteralPath $divertDll.FullName -Destination (Join-Path $RuntimeRoot 'WinDivert.dll') -Force
    Copy-Item -LiteralPath $divertSys.FullName -Destination (Join-Path $RuntimeRoot 'WinDivert64.sys') -Force
    Remove-Item -LiteralPath $extract -Recurse -Force
}

Push-Location $GuiRoot
try {
    cargo build --release
    $artifact = Join-Path $TargetRoot 'goodbyedpi-control.exe'
    if (-not (Test-Path -LiteralPath $artifact)) {
        throw "Cargo did not produce $artifact"
    }
    Write-Host "Built single-file executable: $artifact"
} finally {
    Pop-Location
}

