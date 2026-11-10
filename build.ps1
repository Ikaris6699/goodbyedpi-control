[CmdletBinding()]
param([switch]$SkipRuntimeDownload)

$ErrorActionPreference = 'Stop'
$GuiRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$RuntimeRoot = Join-Path $GuiRoot 'assets\runtime'
$TargetRoot = Join-Path $GuiRoot 'target\release'
New-Item -ItemType Directory -Force -Path $RuntimeRoot | Out-Null

if (-not $SkipRuntimeDownload) {
    $version = '0.2.3rc3-2'
    $url = "https://github.com/ValdikSS/GoodbyeDPI/releases/download/0.2.3rc3/goodbyedpi-$version.zip"
    $expectedSha256 = '37F96B32D050DADCC930A639EBA68E1CCD57ED5C04A5F77DFCA908F01905A4C5'
    $download = Join-Path $env:TEMP "goodbyedpi-$version.zip"
    $extract = Join-Path $env:TEMP ('goodbyedpi-release-' + [guid]::NewGuid().ToString('N'))
    Invoke-WebRequest -Uri $url -OutFile $download
    $actualSha256 = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash
    if ($actualSha256 -ne $expectedSha256) { throw "GoodbyeDPI release SHA256 mismatch: $actualSha256" }
    Expand-Archive -LiteralPath $download -DestinationPath $extract -Force
    $x64 = Join-Path $extract "goodbyedpi-$version\x86_64"
    foreach ($name in @('goodbyedpi.exe', 'WinDivert.dll', 'WinDivert64.sys')) {
        $source = Join-Path $x64 $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "Missing x86_64 runtime file: $source" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $RuntimeRoot $name) -Force
    }
    Remove-Item -LiteralPath $extract -Recurse -Force
}

Push-Location $GuiRoot
try {
    cargo test
    if ($LASTEXITCODE -ne 0) { throw 'Rust unit tests failed.' }
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
    $artifact = Join-Path $TargetRoot 'goodbyedpi-control.exe'
    if (-not (Test-Path -LiteralPath $artifact)) { throw "Cargo did not produce $artifact" }
    Write-Host "Built single-file executable: $artifact"
} finally { Pop-Location }
