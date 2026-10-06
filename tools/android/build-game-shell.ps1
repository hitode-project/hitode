param(
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$TargetDir = (Join-Path $PSScriptRoot '../../target/android-a0'),
    [string]$Output = (Join-Path $PSScriptRoot '../../target/android-shell'),
    [string]$JavaHome = $env:JAVA_HOME,
    [string[]]$Features = @(),
    [switch]$BuildOnly,
    [switch]$SkipPackage
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
if (-not $Sdk -or -not (Test-Path -LiteralPath $Sdk -PathType Container)) { throw "Missing Android SDK: $Sdk" }
if (-not $Ndk -or -not (Test-Path -LiteralPath (Join-Path $Ndk 'source.properties') -PathType Leaf)) { throw "Missing Android NDK: $Ndk" }
if (-not (Test-Path -LiteralPath (Join-Path $repo 'apps/adv-android/Cargo.toml'))) { throw 'Missing apps/adv-android/Cargo.toml' }
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$environmentNames = @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'CARGO_TARGET_DIR', 'ADV_ANDROID_SHELL_BUILD')
$previousEnvironment = @{}
foreach ($name in $environmentNames) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location $repo
try {
    $env:ANDROID_NDK_HOME = $Ndk
    $env:ANDROID_NDK_ROOT = $Ndk
    $env:ANDROID_HOME = $Sdk
    $env:ANDROID_SDK_ROOT = $Sdk
    $env:CARGO_TARGET_DIR = $TargetDir
    $revision = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Git revision' }
    $dirtyFiles = @(& git status --porcelain --untracked-files=no)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Git working tree state' }
    $env:ADV_ANDROID_SHELL_BUILD = "$($revision.Substring(0, 12))-local-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
    $buildArguments = @('ndk', '-t', 'arm64-v8a', '--platform', '29', 'build', '-p', 'adv-android', '--release')
    if ($Features.Count) { $buildArguments += @('--features', ($Features -join ',')) }
    & cargo @buildArguments 2>&1 | Tee-Object -FilePath (Join-Path $Output 'native-build.log')
    if ($LASTEXITCODE -ne 0) { throw "Android game shell build failed: $LASTEXITCODE" }
    $library = Join-Path $TargetDir 'aarch64-linux-android/release/libadv_android.so'
    if (-not (Test-Path -LiteralPath $library -PathType Leaf)) { throw "Build did not produce $library" }
    $sourceFiles = @()
    foreach ($scope in @('apps/adv-android', 'tools/android/game-shell')) {
        foreach ($file in Get-ChildItem -LiteralPath (Join-Path $repo $scope) -Recurse -File) {
            $sourceFiles += [ordered]@{ path = $file.FullName.Substring($repo.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
        }
    }
    foreach ($relative in @('Cargo.toml', 'Cargo.lock', 'tools/android/build-game-shell.ps1', 'tools/android/package-game-shell.ps1')) {
        $sourceFiles += [ordered]@{ path = $relative; sha256 = (Get-FileHash -LiteralPath (Join-Path $repo $relative) -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    $rustVersion = @(& rustc -Vv)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Rust compiler version' }
    $nativeInfo = [ordered]@{
        build_identifier = $env:ADV_ANDROID_SHELL_BUILD
        source_revision = $revision
        tracked_working_tree_changes = $dirtyFiles
        source_files = $sourceFiles
        cargo_arguments = $buildArguments
        rustc = $rustVersion
        ndk_properties = @(Get-Content -LiteralPath (Join-Path $Ndk 'source.properties'))
        target_directory = $TargetDir
        native_library = $library
        native_library_sha256 = (Get-FileHash -LiteralPath $library -Algorithm SHA256).Hash.ToLowerInvariant()
        built_at_utc = [DateTime]::UtcNow.ToString('o')
    }
    $nativeInfoPath = Join-Path $Output 'native-build-info.json'
    $nativeInfo | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $nativeInfoPath -Encoding UTF8
    if ($BuildOnly -or $SkipPackage) {
        Get-Item -LiteralPath $library | Select-Object FullName, Length
    } else {
        & (Join-Path $PSScriptRoot 'package-game-shell.ps1') -Library $library -Output (Join-Path $Output 'apk-game-shell') -Keystore (Join-Path $Output 'debug.keystore') -Sdk $Sdk -Ndk $Ndk -JavaHome $JavaHome -BuildIdentifier $env:ADV_ANDROID_SHELL_BUILD -NativeBuildInfo $nativeInfoPath
    }
} finally {
    Pop-Location
    foreach ($name in $environmentNames) {
        if ($null -eq $previousEnvironment[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process')
        }
    }
}
