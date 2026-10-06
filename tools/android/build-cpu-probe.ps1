param(
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$TargetDir = (Join-Path $PSScriptRoot '..\..\target\android-a0'),
    [string]$JavaHome = $env:JAVA_HOME,
    [switch]$SkipPackage
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
if (-not (Test-Path -LiteralPath (Join-Path $Ndk 'source.properties'))) { throw "Missing Android NDK: $Ndk" }
if (-not (Test-Path -LiteralPath $Sdk)) { throw "Missing Android SDK: $Sdk" }
$environmentNames = @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'CARGO_TARGET_DIR', 'ADV_ANDROID_PROBE_BUILD')
$previous = @{}
foreach ($name in $environmentNames) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location $repo
try {
    $env:ANDROID_NDK_HOME = $Ndk
    $env:ANDROID_NDK_ROOT = $Ndk
    $env:ANDROID_HOME = $Sdk
    $env:ANDROID_SDK_ROOT = $Sdk
    $env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($TargetDir)
    $revision = (& git rev-parse --short=12 HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Git revision' }
    $env:ADV_ANDROID_PROBE_BUILD = "$revision-local-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
    # NDK r28 provides 16 KiB ELF alignment by default. Keep existing Cargo flags
    # so the A0 dependency cache can be reused; packaging verifies every LOAD.
    & cargo ndk -t arm64-v8a --platform 29 build -p adv-android-probe --release
    if ($LASTEXITCODE -ne 0) { throw "Android CPU probe build failed: $LASTEXITCODE" }
    $library = Join-Path $env:CARGO_TARGET_DIR 'aarch64-linux-android\release\libadv_android_probe.so'
    if (-not $SkipPackage) {
        & (Join-Path $PSScriptRoot 'package-cpu-probe.ps1') -Library $library -Output (Join-Path $env:CARGO_TARGET_DIR 'apk-cpu-probe') -Sdk $Sdk -Ndk $Ndk -JavaHome $JavaHome -BuildIdentifier $env:ADV_ANDROID_PROBE_BUILD
    } else {
        Get-Item -LiteralPath $library | Select-Object FullName, Length
    }
} finally {
    Pop-Location
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
