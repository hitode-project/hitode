<#
.SYNOPSIS
Build and run synthetic adv-cpu tests on an authorized Android ARM64 device.
.DESCRIPTION
Inputs are integration target names and unit-test filters, never game assets.
ExecutionExperiments selects A/B/C/D. WhatIf prints the build/device plan as
JSON without tools, files or a device; BuildOnly cross-links without adb.
Device runs retain build/test logs, hashes and result counts in Output.
.EXAMPLE
./tools/android/run-cpu-tests.ps1 -ExecutionExperiments -WhatIf
.EXAMPLE
./tools/android/run-cpu-tests.ps1 -ExecutionExperiments -Serial '<device-serial>'
#>
param(
    [string]$Serial = '',
    [int]$AdbPort = 5037,
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$TargetDir = (Join-Path $PSScriptRoot '../../target/android-a0'),
    [string]$Output = (Join-Path $PSScriptRoot ('../../target/arm-a0/' + (Get-Date -Format 'yyyyMMdd-HHmmss'))),
    [string[]]$TestName = @('execution_contract', 'host_interrupts', 'loop_versioning', 'address_space'),
    [string[]]$UnitFilter = @(),
    [ValidateRange(10, 600)][int]$TimeoutSeconds = 180,
    [switch]$ExecutionExperiments,
    [switch]$BuildOnly,
    [switch]$WhatIf,
    [ValidateSet('off', '1', 'strict')][string]$JitVerify = 'off'
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if ($ExecutionExperiments) {
    if ($PSBoundParameters.ContainsKey('TestName') -or $PSBoundParameters.ContainsKey('UnitFilter')) {
        throw '-ExecutionExperiments cannot be combined with -TestName or -UnitFilter.'
    }
    $TestName = @('tier1')
    $UnitFilter = @('virtual_memory', 'fault_recovery', 'tier1::native::publication_tests', 'compile::x87::tests')
}
$UnitFilter = @($UnitFilter | Where-Object { $_ -ne '' } | Select-Object -Unique)
$TestName = @($TestName | Select-Object -Unique)
if ((-not $TestName.Count -and -not $UnitFilter.Count) -or ($TestName | Where-Object { $_ -notmatch '^[a-zA-Z0-9_]+$' }) -or ($UnitFilter | Where-Object { $_ -notmatch '^[a-zA-Z0-9_:]+$' })) {
    throw 'Select integration-test file names and/or a simple Rust unit-test filter.'
}
foreach ($test in $TestName) {
    if (-not (Test-Path -LiteralPath (Join-Path $repo "crates/adv-cpu/tests/$test.rs") -PathType Leaf)) {
        throw "Unknown adv-cpu integration target: $test"
    }
}
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$cargoArgs = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'test', '--locked', '-p', 'adv-cpu', '--release', '--no-run', '--target-dir', $TargetDir)
foreach ($test in $TestName) { $cargoArgs += @('--test', $test) }
if ($UnitFilter.Count) { $cargoArgs += '--lib' }
$targets = @($TestName)
if ($UnitFilter.Count) { $targets += 'adv_cpu' }
$runs = @(
    foreach ($test in $TestName) { [pscustomobject]@{ id = $test; target = $test; filter = '' } }
    for ($i = 0; $i -lt $UnitFilter.Count; $i++) {
        $id = if ($UnitFilter.Count -eq 1) { 'adv_cpu' } else { "adv_cpu-$($i + 1)" }
        [pscustomobject]@{ id = $id; target = 'adv_cpu'; filter = $UnitFilter[$i] }
    }
)
function Get-DeviceCommand($remote, $run) {
    # Parent tests exec their own deployed ELF and validate each child's status.
    # Never carry a host runner or an individual fault case into the parent.
    $clear = 'unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER CARGO_TARGET_AARCH64_LINUX_ANDROID_RUNNER ADV_CPU_DIRECT_PROBE ADV_CONTROLLED_FAULT_CASE ADV_JIT_VERIFY'
    $verify = if ($JitVerify -eq 'off') { '' } else { "ADV_JIT_VERIFY=$JitVerify " }
    "cd '$remote' && $clear && ${verify}timeout $TimeoutSeconds './$($run.target)' $($run.filter) --test-threads=1 --nocapture"
}
if ($WhatIf) {
    [ordered]@{
        mode = 'dry-run'; build_only = [bool]$BuildOnly; cargo_arguments = $cargoArgs; targets = $targets
        build_environment = @{ ANDROID_NDK_HOME = $Ndk; CMAKE_TOOLCHAIN_FILE = (Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake'); CARGO_BUILD_JOBS = '2' }
        serial = $Serial; adb_port = $AdbPort; jit_verify = $JitVerify
        runs = @($runs | ForEach-Object { [ordered]@{ id = $_.id; target = $_.target; filter = $_.filter; shell = (Get-DeviceCommand '/data/local/tmp/adv-host-cpu-DRYRUN' $_) } })
    } | ConvertTo-Json -Depth 6
    return
}
if (-not $Ndk) { throw 'Set ANDROID_NDK_HOME or pass -Ndk.' }
foreach ($path in @((Join-Path $Ndk 'source.properties'), 'C:/msys64/usr/bin/sh.exe')) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing build tool: $path" }
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path -LiteralPath $Output).Path
if (-not $BuildOnly) {
    if (-not $Sdk) { throw 'Set ANDROID_HOME or pass -Sdk.' }
    $adb = Join-Path $Sdk 'platform-tools/adb.exe'
    if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) { throw "Missing device tool: $adb" }
    $devices = @(& $adb -P $AdbPort devices)
    if ($LASTEXITCODE -ne 0) { throw 'adb device discovery failed' }
    if (-not $Serial) {
        $ready = @($devices | Where-Object { $_ -match '^\S+\s+device$' } | ForEach-Object { ($_ -split '\s+')[0] })
        if ($ready.Count -ne 1) { throw 'Pass -Serial when exactly one authorized device is not available.' }
        $Serial = $ready[0]
    }
    $adbArgs = @('-P', "$AdbPort", '-s', $Serial)
    $device = [ordered]@{}
    foreach ($property in @('ro.product.model', 'ro.build.version.release', 'ro.build.version.sdk', 'ro.product.cpu.abilist')) {
        $value = & $adb @adbArgs shell getprop $property
        if ($LASTEXITCODE -ne 0) { throw "Device property failed: $property" }
        $device[$property] = ($value | Out-String).Trim()
    }
    $pageSize = & $adb @adbArgs shell getconf PAGESIZE
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read host page size' }
    $device['host_page_size'] = ($pageSize | Out-String).Trim()
    $device | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Output 'device.json') -Encoding utf8
}

$envNames = @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'CMAKE_TOOLCHAIN_FILE', 'CARGO_BUILD_JOBS', 'PATH')
$savedEnv = @{}
foreach ($name in $envNames) { $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location $repo
try {
    $env:ANDROID_NDK_HOME = $Ndk
    $env:ANDROID_NDK_ROOT = $Ndk
    $env:ANDROID_HOME = $Sdk
    $env:ANDROID_SDK_ROOT = $Sdk
    $env:CMAKE_TOOLCHAIN_FILE = Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake'
    $env:CARGO_BUILD_JOBS = '2'
    $env:PATH = 'C:/msys64/usr/bin;' + $env:PATH
    $buildLog = Join-Path $Output 'build.log'
    & cargo @cargoArgs 2>&1 | Tee-Object -FilePath $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Android test build failed; see $buildLog" }
    $artifacts = @{}
    # cargo-ndk consumes Cargo's artifact JSON. Use an isolated target directory
    # and refuse ambiguous hashes rather than accidentally running an old build.
    $deps = Join-Path $TargetDir 'aarch64-linux-android/release/deps'
    foreach ($test in $targets) {
        $candidates = @(Get-ChildItem -LiteralPath $deps -File -Filter "$test-*" |
            Where-Object { $_.Name -match ('^' + [regex]::Escape($test) + '-[0-9a-f]+$') })
        if ($candidates.Count -ne 1) { throw "Expected one $test executable; use an isolated -TargetDir when build configurations differ." }
        $artifacts[$test] = $candidates[0].FullName
    }
    $manifest = @($targets | ForEach-Object { [ordered]@{ target = $_; binary = $artifacts[$_]; sha256 = (Get-FileHash -LiteralPath $artifacts[$_] -Algorithm SHA256).Hash } })
    [ordered]@{ context = 'cross-build-only'; artifacts = $manifest } |
        ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output 'build.json') -Encoding utf8
    @(
        'cargo ' + ($cargoArgs -join ' ')
        "ANDROID_NDK_HOME=$Ndk"
        "CMAKE_TOOLCHAIN_FILE=$env:CMAKE_TOOLCHAIN_FILE"
        "CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS"
        "ADV_JIT_VERIFY=$JitVerify (device only)"
        'runner_sha256=' + (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
        'guest_executable_sha256=not-applicable (synthetic tests)'
        $manifest | ForEach-Object { "$($_.target)_sha256=$($_.sha256)" }
    ) | Set-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Encoding utf8
    if ($BuildOnly) {
        Write-Output "CPU cross-build evidence (no device execution): $Output"
        return
    }
    $remote = '/data/local/tmp/adv-host-cpu-' + [Guid]::NewGuid().ToString('N')
    & $adb @adbArgs shell mkdir -p $remote
    if ($LASTEXITCODE -ne 0) { throw 'Cannot create owned device test directory' }
    $results = [Collections.Generic.List[object]]::new()
    foreach ($test in $targets) {
        if (-not $artifacts.ContainsKey($test)) { throw "Missing executable artifact: $test" }
        $binary = $artifacts[$test]
        & $adb @adbArgs push $binary "$remote/$test"
        if ($LASTEXITCODE -ne 0) { throw "Push failed: $test" }
        & $adb @adbArgs shell chmod 700 "$remote/$test"
        if ($LASTEXITCODE -ne 0) { throw "chmod failed: $test" }
    }
    foreach ($run in $runs) {
        $test = $run.target
        $binary = $artifacts[$test]
        $filter = $run.filter
        $command = Get-DeviceCommand $remote $run
        Add-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Value "adb shell $command"
        $log = Join-Path $Output "$($run.id).log"
        & $adb @adbArgs shell $command 2>&1 | Tee-Object -FilePath $log
        $code = $LASTEXITCODE
        $summary = [regex]::Match((Get-Content -LiteralPath $log -Raw), 'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;')
        $passed = if ($summary.Success) { [int]$summary.Groups[1].Value } else { 0 }
        $results.Add([ordered]@{
            test = $test; filter = $filter; binary = $binary; sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
            exit_code = $code; passed = $passed; summary = $summary.Value; log = $log
        })
        [ordered]@{ context = 'adb-shell'; remote_directory = $remote; jit_verify = $JitVerify; results = $results.ToArray() } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output 'results.json') -Encoding utf8
        if ($code -ne 0) { throw "Device test failed ($code): $test; evidence: $Output" }
        if ($passed -eq 0) { throw "No passing tests selected: $test $filter; evidence: $Output" }
    }
    Write-Output "CPU test evidence: $Output"
} finally {
    Pop-Location
    foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name], 'Process') }
}
