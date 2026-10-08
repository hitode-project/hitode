<#
.SYNOPSIS
Build and run synthetic adv-cpu tests on an authorized Android ARM64 device.
.DESCRIPTION
Inputs are integration target names and unit-test filters, never game assets.
ExecutionExperiments selects A/B/C/D. WhatIf prints the build/device plan as
JSON without tools, files or a device; BuildOnly cross-links without adb.
Device runs retain build/test logs, hashes and result counts in Output.
AllTests selects every integration target and unfiltered unit tests, continues
after failures and records empty platform targets. DirectMemory and JitVerify
configure the CPU; Tier1 records the shell selector (fixtures select their modes).
Safepoint selects off/poll independently of verification and direct memory.
CodeValidity selects legacy/event code-cache validation on the device.
Features adds explicit Cargo features. Tier 2 uses the default GIR frontend.
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
    [ValidateRange(60, 1140)][int]$DeviceBudgetSeconds = 1100,
    [switch]$ExecutionExperiments,
    [switch]$AllTests,
    [switch]$NoFailFast,
    [switch]$BuildOnly,
    [switch]$SkipBuild,
    [switch]$WhatIf,
    [ValidateSet('off', '1', 'strict')][string]$JitVerify = 'off',
    [switch]$DirectMemory,
    [ValidateSet('off', 'bridge')][string]$Tier1 = 'off',
    [ValidateSet('off', 'poll')][string]$Safepoint = 'off',
    [ValidateSet('legacy', 'event')][string]$CodeValidity = 'legacy',
    [string[]]$Features = @()
)
$ErrorActionPreference = 'Stop'
if ($BuildOnly -and $SkipBuild) { throw '-BuildOnly and -SkipBuild are mutually exclusive.' }
if ($Features | Where-Object { $_ -notmatch '^[a-zA-Z0-9_-]+(/[a-zA-Z0-9_-]+)?$' }) { throw 'Invalid Cargo feature.' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if ($AllTests) {
    if ($ExecutionExperiments -or $PSBoundParameters.ContainsKey('TestName') -or $PSBoundParameters.ContainsKey('UnitFilter')) {
        throw '-AllTests cannot be combined with other test selectors.'
    }
    $TestName = @(Get-ChildItem -LiteralPath (Join-Path $repo 'crates/adv-cpu/tests') -Filter '*.rs' -File | Sort-Object Name | ForEach-Object { $_.BaseName })
    $UnitFilter = @('')
    $NoFailFast = $true
}
if ($ExecutionExperiments) {
    if ($PSBoundParameters.ContainsKey('TestName') -or $PSBoundParameters.ContainsKey('UnitFilter')) {
        throw '-ExecutionExperiments cannot be combined with -TestName or -UnitFilter.'
    }
    $TestName = @('tier1')
    $UnitFilter = @('virtual_memory', 'fault_recovery', 'tier1::native::publication_tests', 'compile::x87::tests')
}
$UnitFilter = @($UnitFilter | Where-Object { $AllTests -or $_ -ne '' } | Select-Object -Unique)
$TestName = @($TestName | Select-Object -Unique)
if ((-not $TestName.Count -and -not $UnitFilter.Count) -or ($TestName | Where-Object { $_ -notmatch '^[a-zA-Z0-9_]+$' }) -or ($UnitFilter | Where-Object { -not $AllTests -and $_ -notmatch '^[a-zA-Z0-9_:]+$' })) {
    throw 'Select integration-test file names and/or a simple Rust unit-test filter.'
}
foreach ($test in $TestName) {
    if (-not (Test-Path -LiteralPath (Join-Path $repo "crates/adv-cpu/tests/$test.rs") -PathType Leaf)) {
        throw "Unknown adv-cpu integration target: $test"
    }
}
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$cargoArgs = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'test', '--locked', '-p', 'adv-cpu', '--release', '--no-run', '--target-dir', $TargetDir)
if ($Features.Count) { $cargoArgs += @('--features', ($Features -join ',')) }
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
    $clear = 'unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER CARGO_TARGET_AARCH64_LINUX_ANDROID_RUNNER ADV_CPU_DIRECT_PROBE ADV_CONTROLLED_FAULT_CASE ADV_JIT_VERIFY ADV_DIRECT_MEMORY ADV_TIER1 ADV_GIR'
    $verify = if ($JitVerify -eq 'off') { '' } else { "ADV_JIT_VERIFY=$JitVerify " }
    $direct = if ($DirectMemory) { 'ADV_DIRECT_MEMORY=1 ' } else { '' }
    $entry = if ($run.PSObject.Properties['executable']) { $run.executable } else { $run.target }
    "cd '$remote' && $clear && TMPDIR='$remote' CARGO_TARGET_DIR='$remote/target' ADV_CODE_VALIDITY=$CodeValidity ADV_SAFEPOINT=$Safepoint ADV_TIER1=$Tier1 $direct${verify}timeout $TimeoutSeconds './$entry' $($run.filter) --test-threads=1 --nocapture"
}
if ($WhatIf) {
    [ordered]@{
        mode = 'dry-run'; build_only = [bool]$BuildOnly; skip_build = [bool]$SkipBuild; cargo_arguments = $cargoArgs; targets = $targets
        build_environment = @{ ANDROID_NDK_HOME = $Ndk; CMAKE_TOOLCHAIN_FILE = (Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake'); CARGO_BUILD_JOBS = '2' }
        serial = $Serial; adb_port = $AdbPort; jit_verify = $JitVerify; direct_memory = [bool]$DirectMemory; tier1 = $Tier1; all_tests = [bool]$AllTests
        features = $Features
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
function Invoke-AdbRetry([string[]]$Arguments, [string]$Label) {
    # Only repeat idempotent queries, file transfers and operations on our own
    # directory. Never retry a test invocation after an uncertain execution.
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        if ($deviceClock -and $deviceClock.Elapsed.TotalSeconds -gt $DeviceBudgetSeconds - 45) {
            throw 'Device session budget exhausted during transport; retry under a new lock.'
        }
        $lines = @(& $adb -H 127.0.0.1 -P $AdbPort @Arguments 2>&1)
        if ($LASTEXITCODE -eq 0) { return $lines }
        if ($attempt -eq 4) { throw "$Label failed after transport retries: $($lines -join ' ')" }
        Write-Warning "${Label}: transient infrastructure error; retrying in 30 seconds."
        Start-Sleep -Seconds 30
    }
}
if (-not $BuildOnly) {
    $deviceClock = [Diagnostics.Stopwatch]::StartNew()
    if (-not $Sdk) { throw 'Set ANDROID_HOME or pass -Sdk.' }
    $adb = Join-Path $Sdk 'platform-tools/adb.exe'
    if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) { throw "Missing device tool: $adb" }
    $devices = @(Invoke-AdbRetry @('devices') 'Device discovery')
    if (-not $Serial) {
        $ready = @($devices | Where-Object { $_ -match '^\S+\s+device$' } | ForEach-Object { ($_ -split '\s+')[0] })
        if ($ready.Count -ne 1) { throw 'Pass -Serial when exactly one authorized device is not available.' }
        $Serial = $ready[0]
    }
    $adbArgs = @('-H', '127.0.0.1', '-P', "$AdbPort", '-s', $Serial)
    $device = [ordered]@{}
    foreach ($property in @('ro.product.model', 'ro.build.version.release', 'ro.build.version.sdk', 'ro.product.cpu.abilist')) {
        $value = Invoke-AdbRetry @('-s', $Serial, 'shell', 'getprop', $property) 'Device property'
        $device[$property] = ($value | Out-String).Trim()
    }
    $pageSize = Invoke-AdbRetry @('-s', $Serial, 'shell', 'getconf', 'PAGESIZE') 'Device page size'
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
    if (-not $SkipBuild) {
        & cargo @cargoArgs 2>&1 | Tee-Object -FilePath $buildLog
        if ($LASTEXITCODE -ne 0) { throw "Android test build failed; see $buildLog" }
    }
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
        "build_skipped=$([bool]$SkipBuild)"
        "ANDROID_NDK_HOME=$Ndk"
        "CMAKE_TOOLCHAIN_FILE=$env:CMAKE_TOOLCHAIN_FILE"
        "CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS"
        "ADV_JIT_VERIFY=$JitVerify (device only)"
        "ADV_DIRECT_MEMORY=$([int][bool]$DirectMemory) (device only)"
        "ADV_TIER1=$Tier1 (shell selector; CPU fixtures select their own mode)"
        "ADV_SAFEPOINT=$Safepoint (device only)"
        'runner_sha256=' + (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
        'guest_executable_sha256=not-applicable (synthetic tests)'
        $manifest | ForEach-Object { "$($_.target)_sha256=$($_.sha256)" }
    ) | Set-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Encoding utf8
    if ($BuildOnly) {
        Write-Output "CPU cross-build evidence (no device execution): $Output"
        return
    }
    $remote = '/data/local/tmp/adv-host-cpu-' + [Guid]::NewGuid().ToString('N')
    Set-Content -LiteralPath (Join-Path $Output 'remote-directory.txt') -Value $remote
    Invoke-AdbRetry @('-s', $Serial, 'shell', 'mkdir', '-p', $remote) 'Device directory'
    $results = [Collections.Generic.List[object]]::new()
    # One multi-file transfer avoids a connection per ELF. All sources are
    # regular files with unique Cargo names, so repeating a partial push cannot
    # create nested directories or overwrite another task's data.
    $sources = @($targets | ForEach-Object { $artifacts[$_] })
    Invoke-AdbRetry (@('-s', $Serial, 'push', '--sync') + $sources + @("$remote/")) 'Push'
    Invoke-AdbRetry @('-s', $Serial, 'shell', "chmod 700 '$remote'/*") 'chmod'
    foreach ($run in $runs) {
        $remaining = [int]($DeviceBudgetSeconds - $deviceClock.Elapsed.TotalSeconds)
        if ($remaining -lt 10) { throw "Device session budget exhausted; resume remaining targets from $Output" }
        $TimeoutSeconds = [Math]::Min($TimeoutSeconds, $remaining)
        $test = $run.target
        $binary = $artifacts[$test]
        $run | Add-Member -NotePropertyName executable -NotePropertyValue ([IO.Path]::GetFileName($binary))
        $filter = $run.filter
        $command = Get-DeviceCommand $remote $run
        Add-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Value "adb shell $command"
        $log = Join-Path $Output "$($run.id).log"
        for ($attempt = 1; $attempt -le 4; $attempt++) {
            & $adb @adbArgs shell $command 2>&1 | Tee-Object -FilePath $log
            $code = $LASTEXITCODE
            $transport = (Get-Content -LiteralPath $log -Raw) -match 'cannot connect to daemon|device offline|device not found'
            if (-not $transport) { break }
            Copy-Item -LiteralPath $log -Destination (Join-Path $Output "$($run.id)-transport-$attempt.log")
            if ($attempt -eq 4 -or $deviceClock.Elapsed.TotalSeconds -gt $DeviceBudgetSeconds - $TimeoutSeconds - 30) {
                throw "Device transport failed before test execution: $test"
            }
            Start-Sleep -Seconds 30
        }
        $content = Get-Content -LiteralPath $log -Raw
        $summary = [regex]::Match($content, 'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;')
        $passed = if ($summary.Success) { [int]$summary.Groups[1].Value } else { 0 }
        $failed = if ($summary.Success) { [int]$summary.Groups[2].Value } else { 0 }
        $ignored = if ($summary.Success) { [int]$summary.Groups[3].Value } else { 0 }
        $empty = $summary.Success -and $code -eq 0 -and $passed -eq 0 -and $failed -eq 0 -and -not $filter
        $ok = $summary.Success -and $code -eq 0 -and ($passed -gt 0 -or $empty)
        $failedNames = @([regex]::Matches($content, '(?m)^    (\S+)\r?$') | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
        $results.Add([ordered]@{
            test = $test; filter = $filter; binary = $binary; sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
            exit_code = $code; passed = $passed; failed = $failed; ignored = $ignored; skipped_empty_target = $empty; ok = $ok; failed_names = $failedNames; summary = $summary.Value; log = $log
        })
        [ordered]@{ context = 'adb-shell'; remote_directory = $remote; jit_verify = $JitVerify; direct_memory = [bool]$DirectMemory; tier1 = $Tier1; bridge_scope = 'Explicit tier1_bridge fixtures; environment does not override Executor policies'; results = $results.ToArray() } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output 'results.json') -Encoding utf8
        if (-not $ok -and -not $NoFailFast) { throw "Device test failed ($code): $test $filter; evidence: $Output" }
        if ($empty) { Write-Output "Skipped empty platform target: $test" }
    }
    if (@($results | Where-Object { -not $_.ok }).Count) { throw "Device suite contains failures; evidence: $Output" }
    Write-Output "CPU test evidence: $Output"
} finally {
    if ($remote -and $remote -match '^/data/local/tmp/adv-host-cpu-[a-f0-9]{32}$') {
        # Cleanup has its own bounded transport retries even after a run timeout.
        $deviceClock = $null
        try { Invoke-AdbRetry @('-s', $Serial, 'shell', 'rm', '-rf', $remote) 'Device cleanup' }
        catch { Write-Warning "Owned device directory cleanup needs retry: $remote" }
    }
    Pop-Location
    foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name], 'Process') }
}
