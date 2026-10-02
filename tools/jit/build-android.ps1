param(
    [Parameter(Mandatory = $true)][string]$Ndk,
    [string]$MsysBin = 'C:\msys64\usr\bin'
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath (Join-Path $Ndk 'source.properties'))) { throw 'Invalid NDK directory' }
if (-not (Test-Path -LiteralPath (Join-Path $MsysBin 'sh.exe'))) { throw 'Unicorn requires MSYS sh on Windows' }
$env:ANDROID_NDK_HOME = $Ndk
$env:ANDROID_NDK_ROOT = $Ndk
$env:CMAKE_TOOLCHAIN_FILE = (Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake')
$env:PATH = $MsysBin + ';' + $env:PATH
$env:CARGO_TARGET_DIR = (Join-Path $PSScriptRoot '..\..\target\android')
Push-Location (Join-Path $PSScriptRoot '..\..')
try {
    cargo ndk -t arm64-v8a --platform 29 build --release -p adv-jit -p adv-runtime --examples
    if ($LASTEXITCODE -ne 0) { throw "Android build failed: $LASTEXITCODE" }
    cargo ndk -t arm64-v8a --platform 29 build --release -p adv-run --bin adv-run
    if ($LASTEXITCODE -ne 0) { throw "Android adv-run build failed: $LASTEXITCODE" }
    Write-Output "Android headless runner: $env:CARGO_TARGET_DIR\aarch64-linux-android\release\adv-run (run with --headless)"
} finally {
    Pop-Location
}
