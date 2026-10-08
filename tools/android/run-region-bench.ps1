<#
.SYNOPSIS
Build and run the anonymous, headless ARM64 region benchmark.
.DESCRIPTION
Inputs: an optional locally staged private capture Corpus, hexadecimal CpuMask,
isolated TargetDir/Output and the shared atomic DeviceLock directory. Output:
anonymous benchmark CSV/log, commands and binary/runner SHA-256. No APK or UI.
BuildOnly and WhatIf never access a device. SkipBuild reuses this task's ELF.
The caller owns TEMP/TMP and storage policy. Existing adb server port 5037 only.
.EXAMPLE
./tools/android/run-region-bench.ps1 -BuildOnly -TargetDir $env:CARGO_TARGET_DIR -Output $env:ADV_BENCH_OUTPUT
.EXAMPLE
./tools/android/run-region-bench.ps1 -SkipBuild -TargetDir $env:CARGO_TARGET_DIR -Output $env:ADV_BENCH_OUTPUT -Corpus $env:ADV_GIR_LIFT_CORPUS -CpuMask 80 -DeviceLock $env:ADV_DEVICE_LOCK
#>
param(
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [Parameter(Mandatory)][string]$TargetDir,
    [Parameter(Mandatory)][string]$Output,
    [string]$Corpus = '',
    [ValidatePattern('^[0-9a-fA-F]+$')][string]$CpuMask = '80',
    [string]$DeviceLock = $env:ADV_DEVICE_LOCK,
    [ValidateRange(1, 31)][int]$Samples = 7,
    [ValidateRange(1, 100000)][int]$Loops = 10000,
    [ValidateRange(1, 1000)][int]$Repeats = 4,
    [ValidateRange(1, 16)][int]$PrivateLimit = 3,
    [ValidateRange(10, 600)][int]$TimeoutSeconds = 300,
    [switch]$BuildOnly,
    [switch]$SkipBuild,
    [switch]$WhatIf
)
$ErrorActionPreference = 'Stop'
if ($BuildOnly -and $SkipBuild) { throw 'BuildOnly and SkipBuild are mutually exclusive.' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$Output = [IO.Path]::GetFullPath($Output)
$cargoArgs = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'build', '--locked', '-p', 'adv-cpu', '--release', '--example', 'host_region_bench', '--target-dir', $TargetDir)
function Get-Command([string]$Remote) {
    $corpusEnv = if ($Corpus) { "ADV_GIR_LIFT_CORPUS='$Remote/corpus' " } else { '' }
    "unset ADV_JIT_VERIFY ADV_DIRECT_MEMORY ADV_HOST_FEATURES ADV_REGION_BENCH_EXPORT_CORPUS ADV_INTERP_PROFILE_DIR ADV_INTERP_TRACE_DIR ADV_TIER1_CENSUS; ${corpusEnv}ADV_REGION_BENCH_SAMPLES=$Samples ADV_REGION_BENCH_LOOPS=$Loops ADV_REGION_BENCH_REPEATS=$Repeats ADV_REGION_BENCH_PRIVATE_LIMIT=$PrivateLimit taskset $CpuMask timeout $TimeoutSeconds '$Remote/host_region_bench'"
}
if ($WhatIf) {
    [ordered]@{ build_only = [bool]$BuildOnly; skip_build = [bool]$SkipBuild; cargo_arguments = $cargoArgs;
        cpu_mask = $CpuMask; adb_port = 5037; shell = Get-Command '/data/local/tmp/adv-region-bench-DRYRUN';
        cleanup = $true; cooldown_seconds = 60; device_lock = $DeviceLock } | ConvertTo-Json -Depth 4
    return
}
if (-not $BuildOnly -and -not $DeviceLock) { throw 'Pass the shared DeviceLock directory.' }
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$binary = Join-Path $TargetDir 'aarch64-linux-android/release/examples/host_region_bench'
if (-not $SkipBuild) {
    $names = @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'CMAKE_TOOLCHAIN_FILE', 'CARGO_BUILD_JOBS', 'PATH')
    $saved = @{}
    foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    Push-Location $repo
    try {
        $env:ANDROID_NDK_HOME = $Ndk
        $env:ANDROID_NDK_ROOT = $Ndk
        $env:CMAKE_TOOLCHAIN_FILE = Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake'
        $env:CARGO_BUILD_JOBS = '2'
        $env:PATH = 'C:/msys64/usr/bin;' + $env:PATH
        & cargo @cargoArgs *> (Join-Path $Output 'build.log')
        if ($LASTEXITCODE -ne 0) { throw 'Benchmark cross-build failed; see build.log.' }
    } finally {
        Pop-Location
        foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Benchmark ELF missing.' }
@('cargo ' + ($cargoArgs -join ' '), "cpu_mask=$CpuMask", 'frequency_locked=false',
    "samples=$Samples loops=$Loops repeats=$Repeats private_limit=$PrivateLimit",
    'binary_sha256=' + (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash,
    'runner_sha256=' + (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash) |
    Set-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Encoding utf8
if ($BuildOnly) { Write-Output 'Region benchmark cross-build complete.'; return }
$adb = Join-Path $Sdk 'platform-tools/adb.exe'
if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) { throw 'adb missing.' }
if ($Corpus -and -not (Test-Path -LiteralPath $Corpus -PathType Container)) { throw 'Local staged corpus missing.' }
$locked = $false
$remote = '/data/local/tmp/adv-region-bench-' + [Guid]::NewGuid().ToString('N')
$clock = $null
function Invoke-Adb([string[]]$Arguments) {
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        if ($clock -and $clock.Elapsed.TotalSeconds -gt 1000) { throw 'Device session budget exhausted.' }
        $lines = @(& $adb -H 127.0.0.1 -P 5037 @Arguments 2>&1)
        if ($LASTEXITCODE -eq 0) { return $lines }
        if ($attempt -eq 4) { throw 'Device transport failed after retries.' }
        Write-Warning 'Transient device transport failure; retrying in 30 seconds.'
        Start-Sleep -Seconds 30
    }
}
try {
    # Always leave a full cooldown before acquiring a subsequent task session.
    Start-Sleep -Seconds 60
    while (-not $locked) {
        try {
            New-Item -ItemType Directory -Path $DeviceLock -ErrorAction Stop | Out-Null
            $locked = $true
        } catch {
            if (-not (Test-Path -LiteralPath $DeviceLock -PathType Container)) { throw }
            Start-Sleep -Seconds 30
        }
    }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Set-Content -LiteralPath (Join-Path $DeviceLock 'owner.txt') -Value 'feat'
    # A raw serial is consumed locally, never recorded or printed.
    $devices = @(Invoke-Adb @('devices'))
    $ready = @($devices | Where-Object { $_ -match '^\S+\s+device$' } | ForEach-Object { ($_ -split '\s+')[0] })
    if ($ready.Count -ne 1) { throw 'Exactly one authorized device is required.' }
    $device = $ready[0]
    Invoke-Adb @('-s', $device, 'shell', 'mkdir', '-p', $remote) | Out-Null
    Invoke-Adb @('-s', $device, 'push', $binary, "$remote/host_region_bench") | Out-Null
    if ($Corpus) { Invoke-Adb @('-s', $device, 'push', $Corpus, "$remote/corpus") | Out-Null }
    Invoke-Adb @('-s', $device, 'shell', 'chmod', '700', "$remote/host_region_bench") | Out-Null
    # Record the bound core's policy/frequency as context; never change it.
    Invoke-Adb @('-s', $device, 'shell', 'cat /sys/devices/system/cpu/online; for p in /sys/devices/system/cpu/cpufreq/policy*; do echo policy; cat $p/related_cpus $p/scaling_max_freq $p/scaling_cur_freq $p/scaling_governor; done') |
        Set-Content -LiteralPath (Join-Path $Output 'frequency-before.txt')
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        $remaining = [int](1000 - $clock.Elapsed.TotalSeconds)
        if ($remaining -lt 10) { throw 'Device session budget exhausted before benchmark.' }
        $TimeoutSeconds = [Math]::Min($TimeoutSeconds, $remaining)
        $command = Get-Command $remote
        Add-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Value "adb shell $command"
        & $adb -H 127.0.0.1 -P 5037 -s $device shell $command *> (Join-Path $Output 'bench.csv')
        $code = $LASTEXITCODE
        $log = Get-Content -LiteralPath (Join-Path $Output 'bench.csv') -Raw
        if ($log -notmatch 'cannot connect to daemon|device offline|device not found') { break }
        if ($attempt -eq 4) { throw 'Benchmark transport failed before execution.' }
        Start-Sleep -Seconds 30
    }
    if ($code -ne 0 -or $log -notmatch '# completed accepted=') { throw 'Benchmark failed; see bench.csv.' }
    Write-Output 'Region benchmark completed; anonymous statistics saved in bench.csv.'
} finally {
    if ($locked) {
        try {
            if ($device -and $remote -match '^/data/local/tmp/adv-region-bench-[a-f0-9]{32}$') {
                # Cleanup gets its own bounded transport retries, even on timeout.
                $clock = $null
                Invoke-Adb @('-s', $device, 'shell', 'rm', '-rf', $remote) | Out-Null
            }
        } finally {
            Remove-Item -LiteralPath (Join-Path $DeviceLock 'owner.txt') -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $DeviceLock
        }
    }
}
