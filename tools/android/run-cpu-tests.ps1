param(
    [string]$Serial = '',
    [int]$AdbPort = 5037,
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$TargetDir = (Join-Path $PSScriptRoot '../../target/android-a0'),
    [string]$Output = (Join-Path $PSScriptRoot ('../../target/arm-a0/' + (Get-Date -Format 'yyyyMMdd-HHmmss'))),
    [string[]]$TestName = @('execution_contract', 'host_interrupts', 'loop_versioning', 'address_space'),
    [string]$UnitFilter = '',
    [ValidateRange(10, 600)][int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$adb = Join-Path $Sdk 'platform-tools/adb.exe'
foreach ($path in @($adb, (Join-Path $Ndk 'source.properties'), 'C:/msys64/usr/bin/sh.exe')) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing build/device tool: $path" }
}
if ((-not $TestName.Count -and -not $UnitFilter) -or ($TestName | Where-Object { $_ -notmatch '^[a-zA-Z0-9_]+$' }) -or ($UnitFilter -and $UnitFilter -notmatch '^[a-zA-Z0-9_:]+$')) {
    throw 'Select integration-test file names and/or a simple Rust unit-test filter.'
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path -LiteralPath $Output).Path
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
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
    $cargoArgs = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'test', '-p', 'adv-cpu', '--release', '--no-run', '--target-dir', $TargetDir)
    foreach ($test in $TestName) { $cargoArgs += @('--test', $test) }
    if ($UnitFilter) { $cargoArgs += '--lib' }
    $buildLog = Join-Path $Output 'build.log'
    & cargo @cargoArgs 2>&1 | Tee-Object -FilePath $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Android test build failed; see $buildLog" }
    $artifacts = @{}
    # cargo-ndk consumes Cargo's artifact JSON. Use an isolated target directory
    # and refuse ambiguous hashes rather than accidentally running an old build.
    $deps = Join-Path $TargetDir 'aarch64-linux-android/release/deps'
    $targets = @($TestName)
    if ($UnitFilter) { $targets += 'adv_cpu' }
    foreach ($test in $targets) {
        $candidates = @(Get-ChildItem -LiteralPath $deps -File -Filter "$test-*" |
            Where-Object { $_.Name -match ('^' + [regex]::Escape($test) + '-[0-9a-f]+$') })
        if ($candidates.Count -ne 1) { throw "Expected one $test executable; use an isolated -TargetDir when build configurations differ." }
        $artifacts[$test] = $candidates[0].FullName
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
        $started = [DateTime]::UtcNow
        $filter = if ($test -eq 'adv_cpu') { $UnitFilter } else { '' }
        & $adb @adbArgs shell "cd '$remote' && timeout $TimeoutSeconds './$test' $filter --test-threads=1 --nocapture" 2>&1 |
            Tee-Object -FilePath (Join-Path $Output "$test.log")
        $code = $LASTEXITCODE
        $results.Add([ordered]@{
            test = $test; filter = $filter; binary = $binary; sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
            exit_code = $code; elapsed_seconds = ([DateTime]::UtcNow - $started).TotalSeconds
        })
        [ordered]@{ context = 'adb-shell'; remote_directory = $remote; results = $results.ToArray() } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output 'results.json') -Encoding utf8
        if ($code -ne 0) { throw "Device test failed ($code): $test; evidence: $Output" }
    }
    Write-Output "CPU test evidence: $Output"
} finally {
    Pop-Location
    foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name], 'Process') }
}
