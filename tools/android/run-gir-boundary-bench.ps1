<#
.SYNOPSIS
Build and run the anonymous, synthetic ARM64 GIR boundary fixture.
.DESCRIPTION
Inputs: shared TargetDir, task-local Output/TEMP/TMP, optional CPU mask and the
shared atomic DeviceLock. No private corpus, APK, UI or server lifecycle calls.
Output: staged ELF, build log, anonymous CSV, affinity/frequency context and
cmd.txt with environment and binary/runner SHA-256. BuildOnly/WhatIf never use
a device. SkipBuild uses the staged ELF from this task's preceding BuildOnly.
The owner is g14-sub2. A session includes 60 seconds of pre-lock cooldown,
30-second lock retries, bounded transport retries and finally remote cleanup.
Device commands have deadlines; the lock budget including cleanup is <20 min.
.EXAMPLE
./tools/android/run-gir-boundary-bench.ps1 -BuildOnly -TargetDir $env:ADV_ANDROID_TARGET -Output $env:ADV_BOUNDARY_OUTPUT
.EXAMPLE
./tools/android/run-gir-boundary-bench.ps1 -SkipBuild -TargetDir $env:ADV_ANDROID_TARGET -Output $env:ADV_BOUNDARY_OUTPUT -DeviceLock $env:ADV_DEVICE_LOCK
#>
param(
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [Parameter(Mandatory)][string]$TargetDir,
    [Parameter(Mandatory)][string]$Output,
    [ValidatePattern('^(auto|[0-9a-fA-F]+)$')][string]$CpuMask = 'auto',
    [string]$DeviceLock = $env:ADV_DEVICE_LOCK,
    [ValidateRange(1, 31)][int]$Samples = 7,
    [ValidateRange(1, 100000000)][long]$Loops = 1000000,
    [ValidateRange(10, 300)][int]$TimeoutSeconds = 120,
    [switch]$BuildOnly,
    [switch]$SkipBuild,
    [switch]$WhatIf
)
$ErrorActionPreference = 'Stop'
if ($BuildOnly -and $SkipBuild) { throw 'BuildOnly and SkipBuild are mutually exclusive.' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$Output = [IO.Path]::GetFullPath($Output)
$cargoArgs = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'build', '--locked', '-p', 'adv-cpu', '--release', '--example', 'gir_boundary_bench', '--target-dir', $TargetDir)

function Get-CpuChoices([string[]]$Inventory) {
    $allowedLine = ($Inventory | Where-Object { $_ -match '^Cpus_allowed_list:' } | Select-Object -First 1)
    if (-not $allowedLine) { throw 'Cannot read allowed CPU range.' }
    $allowed = @{}
    foreach ($part in ($allowedLine -replace '^Cpus_allowed_list:\s*', '').Trim().Split(',')) {
        if ($part -notmatch '^\d+(-\d+)?$') { throw 'Malformed allowed CPU range.' }
        $bounds = $part.Split('-')
        $first = [int]$bounds[0]
        $last = if ($bounds.Count -eq 2) { [int]$bounds[1] } else { $first }
        if ($last -lt $first -or $last -ge 63) { throw 'Unsupported allowed CPU range.' }
        foreach ($core in $first..$last) { $allowed[$core] = $true }
    }
    @($Inventory | ForEach-Object {
        if ($_ -match '^(\d+) (\d+)$' -and $allowed.ContainsKey([int]$Matches[1])) {
            [pscustomobject]@{ core = [int]$Matches[1]; frequency = [long]$Matches[2] }
        }
    } | Sort-Object -Property @{ Expression = 'frequency'; Descending = $true }, @{ Expression = 'core'; Descending = $true })
}

function Get-Command([string]$Remote) {
    if ($Remote -notmatch '^/data/local/tmp/adv-gir-boundary-[a-f0-9]{32}$' -and $Remote -ne '/data/local/tmp/adv-gir-boundary-DRYRUN') { throw 'Invalid remote directory.' }
    "ADV_BOUNDARY_SAMPLES=$Samples ADV_BOUNDARY_LOOPS=$Loops taskset $CpuMask timeout $TimeoutSeconds '$Remote/gir_boundary_bench'"
}

function Try-DeviceLock([string]$Lock) {
    try {
        New-Item -ItemType Directory -Path $Lock -ErrorAction Stop | Out-Null
    } catch {
        if (Test-Path -LiteralPath $Lock -PathType Container) { return $false }
        throw
    }
    # Ownership starts at atomic mkdir, even if writing its metadata fails.
    try {
        Set-Content -LiteralPath (Join-Path $Lock 'owner.txt') -Value 'g14-sub2' -Encoding utf8
    } catch {
        Remove-Item -LiteralPath (Join-Path $Lock 'owner.txt') -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $Lock
        throw
    }
    return $true
}

function Remove-OwnedLock([string]$Lock) {
    $owner = Join-Path $Lock 'owner.txt'
    if ((Get-Content -LiteralPath $owner -Raw).Trim() -ne 'g14-sub2') { throw 'Shared lock ownership changed.' }
    Remove-Item -LiteralPath $owner
    Remove-Item -LiteralPath $Lock
}

function Test-BenchmarkOutput([string]$Text, [int]$ExpectedSamples, [long]$ExpectedLoops) {
    $lines = @($Text -split '\r?\n' | Where-Object { $_ })
    if (@($lines | Where-Object { $_ -notmatch '^# |^case,sample,|^[a-z0-9_]+,\d+,\d+\.\d+,' }).Count) { throw 'Unexpected benchmark output; not saved.' }
    $csv = @($lines | Where-Object { $_ -notmatch '^#' } | ConvertFrom-Csv)
    if ($csv.Count -ne 15 * $ExpectedSamples -or $Text -notmatch '# completed samples=') { throw 'Incomplete benchmark samples.' }
    $groups = @($csv | Group-Object case)
    if ($groups.Count -ne 15) { throw 'Incomplete benchmark cases.' }
    foreach ($group in $groups) {
        if ($group.Count -ne $ExpectedSamples) { throw 'Incomplete case samples.' }
        $ordinals = @($group.Group.sample | Sort-Object -Unique)
        if ($ordinals.Count -ne $ExpectedSamples) { throw 'Duplicate case samples.' }
        foreach ($row in $group.Group) {
            if ([long]$row.verified_boundaries -ne $ExpectedLoops -or [int]$row.full_state_assertions -lt 19) { throw 'Missing full-state validation.' }
        }
    }
    return $lines
}

if ($WhatIf) {
    [ordered]@{ build_only = [bool]$BuildOnly; skip_build = [bool]$SkipBuild;
        cargo_arguments = $cargoArgs; cpu_mask = $CpuMask; adb_port = 5037; owner = 'g14-sub2';
        shell = Get-Command '/data/local/tmp/adv-gir-boundary-DRYRUN';
        cleanup = $true; cooldown_seconds = 60; lock_retry_seconds = 30;
        session_budget_seconds = 1100; transport_deadline_seconds = 45; device_lock = $DeviceLock } | ConvertTo-Json -Depth 4
    return
}
$storageRoot = [IO.Path]::GetPathRoot($Output)
if (-not $env:TEMP -or -not $env:TMP -or [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($env:TEMP)) -ne $storageRoot -or [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($env:TMP)) -ne $storageRoot) { throw 'Set task-local TEMP and TMP on the authorized output volume before running.' }
if ([IO.Path]::GetPathRoot($TargetDir) -ne $storageRoot) { throw 'TargetDir and Output must use the same authorized volume.' }
if (-not $BuildOnly -and -not $DeviceLock) { throw 'Pass the shared DeviceLock directory.' }
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$binary = Join-Path $Output 'gir_boundary_bench'
if (-not $SkipBuild) {
    $names = @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'CMAKE_TOOLCHAIN_FILE', 'CARGO_BUILD_JOBS', 'PATH', 'CARGO_TARGET_DIR')
    $saved = @{}
    foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    Push-Location $repo
    try {
        $env:ANDROID_NDK_HOME = $Ndk
        $env:ANDROID_NDK_ROOT = $Ndk
        $env:CMAKE_TOOLCHAIN_FILE = Join-Path $PSScriptRoot 'arm64-v8a.toolchain.cmake'
        $env:CARGO_BUILD_JOBS = '2'
        $env:CARGO_TARGET_DIR = $TargetDir
        $env:PATH = 'C:/msys64/usr/bin;' + $env:PATH
        & cargo @cargoArgs *> (Join-Path $Output 'build.log')
        if ($LASTEXITCODE -ne 0) { throw 'Boundary cross-build failed; see build.log.' }
        Copy-Item -LiteralPath (Join-Path $TargetDir 'aarch64-linux-android/release/examples/gir_boundary_bench') -Destination $binary
    } finally {
        Pop-Location
        foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Task-staged benchmark ELF missing; run BuildOnly first.' }
@(('cargo ' + ($cargoArgs -join ' ')), "cpu_mask=$CpuMask frequency_locked=false owner=g14-sub2",
    "samples=$Samples loops=$Loops TEMP=$env:TEMP TMP=$env:TMP CARGO_HOME=$env:CARGO_HOME",
    "android_target=$TargetDir timeout_seconds=$TimeoutSeconds",
    ('binary_sha256=' + (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash),
    ('runner_sha256=' + (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash)) |
    Set-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Encoding utf8
if ($BuildOnly) { Write-Output 'Boundary benchmark cross-build complete.'; return }
$DeviceLock = [IO.Path]::GetFullPath($DeviceLock)
$requiredLock = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetPathRoot($Output)) 'adv-tmp/device-lock'))
if ($DeviceLock -ne $requiredLock) { throw 'Use the shared atomic device lock.' }
$adb = Join-Path $Sdk 'platform-tools/adb.exe'
if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) { throw 'adb missing.' }
$locked = $false
$device = $null
$clock = $null
$remote = '/data/local/tmp/adv-gir-boundary-' + [Guid]::NewGuid().ToString('N')

function Invoke-Adb([string[]]$Arguments, [int]$DeadlineSeconds = 45, [switch]$Cleanup) {
    $attempts = if ($Cleanup) { 2 } else { 4 }
    for ($attempt = 1; $attempt -le $attempts; $attempt++) {
        if (-not $Cleanup -and $clock.Elapsed.TotalSeconds + $DeadlineSeconds + 90 -gt 1100) { throw 'Device session budget exhausted.' }
        # Explicit remote-server mode plus an existing-port check prevents adb
        # from creating a replacement local server after a connection failure.
        $client = [Net.Sockets.TcpClient]::new()
        $process = $null
        try {
            if (-not $client.ConnectAsync('127.0.0.1', 5037).Wait(2000)) { throw 'Existing adb server unavailable.' }
            if (-not $client.Connected) { throw 'Existing adb server unavailable.' }
            $info = [Diagnostics.ProcessStartInfo]::new()
            $info.FileName = $adb
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            foreach ($argument in @('-H', '127.0.0.1', '-P', '5037') + $Arguments) { $info.ArgumentList.Add($argument) }
            $process = [Diagnostics.Process]::Start($info)
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            if (-not $process.WaitForExit($DeadlineSeconds * 1000)) {
                $process.Kill($true)
                throw 'Device command deadline exceeded.'
            }
            $text = $stdout.GetAwaiter().GetResult() + $stderr.GetAwaiter().GetResult()
            if ($process.ExitCode -eq 0) { return @($text -split '\r?\n' | Where-Object { $_ }) }
            # Never repeat a started fixture after a correctness/runtime error.
            if ($Arguments[-1] -match 'ADV_BOUNDARY_SAMPLES=' -and $text -notmatch 'cannot connect to daemon|device offline|device not found|cannot connect to') { throw 'Benchmark failed after execution.' }
        } catch {
            if ($_.Exception.Message -eq 'Benchmark failed after execution.') { throw }
            if ($attempt -eq $attempts) { throw 'Device transport failed after bounded retries.' }
        } finally {
            $client.Dispose()
            if ($process) { $process.Dispose() }
        }
        if ($attempt -eq $attempts) { throw 'Device transport failed after bounded retries.' }
        Write-Warning 'Transient device transport failure; retrying.'
        Start-Sleep -Seconds $(if ($Cleanup) { 5 } else { 30 })
    }
}

try {
    Start-Sleep -Seconds 60
    while (-not $locked) {
        $locked = Try-DeviceLock $DeviceLock
        if (-not $locked) { Start-Sleep -Seconds 30 }
    }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    # Serial is consumed locally and is never printed or written to evidence.
    $devices = @(Invoke-Adb @('devices'))
    $ready = @($devices | Where-Object { $_ -match '^\S+\s+device$' } | ForEach-Object { ($_ -split '\s+')[0] })
    if ($ready.Count -ne 1) { throw 'Exactly one authorized device is required.' }
    $device = $ready[0]
    Invoke-Adb @('-s', $device, 'shell', 'mkdir', '-p', $remote) | Out-Null
    Invoke-Adb @('-s', $device, 'push', $binary, "$remote/gir_boundary_bench") | Out-Null
    Invoke-Adb @('-s', $device, 'shell', 'chmod', '700', "$remote/gir_boundary_bench") | Out-Null
    if ($CpuMask -eq 'auto') {
        $inventory = @(Invoke-Adb @('-s', $device, 'shell', 'grep Cpus_allowed_list /proc/self/status; for p in /sys/devices/system/cpu/cpu[0-9]*/cpufreq; do c=${p%/cpufreq}; c=${c##*/cpu}; echo "$c $(cat $p/scaling_max_freq)"; done'))
        $inventory | Set-Content -LiteralPath (Join-Path $Output 'affinity.txt')
        $choices = @(Get-CpuChoices $inventory)
        if (-not $choices.Count) { throw 'No supported permitted CPU.' }
        $masks = @($choices | ForEach-Object { ([long]1 -shl $_.core).ToString('x') })
        $probe = 'for mask in ' + ($masks -join ' ') + '; do if taskset "$mask" true >/dev/null 2>&1; then echo "$mask"; break; fi; done'
        $CpuMask = (Invoke-Adb @('-s', $device, 'shell', $probe) | Out-String).Trim()
        if ($CpuMask -notin $masks) { throw 'No CPU accepted an affinity probe.' }
        $selected = $choices[[Array]::IndexOf($masks, $CpuMask)]
        Add-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Value "resolved_cpu_mask=$CpuMask selected_core=$($selected.core) selected_max_khz=$($selected.frequency)"
    }
    Invoke-Adb @('-s', $device, 'shell', 'for p in /sys/devices/system/cpu/cpufreq/policy*; do echo policy; cat $p/related_cpus $p/scaling_max_freq $p/scaling_cur_freq $p/scaling_governor; done') |
        Set-Content -LiteralPath (Join-Path $Output 'frequency-before.txt')
    $command = Get-Command $remote
    Add-Content -LiteralPath (Join-Path $Output 'cmd.txt') -Value "adb shell $command"
    $result = Invoke-Adb @('-s', $device, 'shell', $command) -DeadlineSeconds ($TimeoutSeconds + 20)
    Test-BenchmarkOutput ($result -join "`n") $Samples $Loops |
        Set-Content -LiteralPath (Join-Path $Output 'bench.csv') -Encoding utf8
    Write-Output 'Boundary benchmark completed; anonymous statistics saved.'
} finally {
    if ($locked) {
        try {
            if ($device -and $remote -match '^/data/local/tmp/adv-gir-boundary-[a-f0-9]{32}$') {
                Invoke-Adb @('-s', $device, 'shell', 'rm', '-rf', $remote) -DeadlineSeconds 30 -Cleanup | Out-Null
            }
        } finally {
            Remove-OwnedLock $DeviceLock
        }
    }
}
