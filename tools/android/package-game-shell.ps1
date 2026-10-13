param(
    [string]$Library = (Join-Path $PSScriptRoot '../../target/android-a0/aarch64-linux-android/release/libadv_android.so'),
    [string]$Output = (Join-Path $PSScriptRoot '../../target/android-shell/apk-game-shell'),
    [string]$Keystore = (Join-Path $PSScriptRoot '../../target/android-shell/debug.keystore'),
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$JavaHome = $env:JAVA_HOME,
    [string]$BuildToolsVersion = '36.0.0',
    [string]$Platform = 'android-36',
    [string]$BuildIdentifier = 'unlabelled',
    [string]$NativeBuildInfo = '',
    [string[]]$AdditionalLibrary = @(),
    [string[]]$AdditionalLibrarySearchPath = @()
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Library -PathType Leaf)) { throw "Missing Android game shell library: $Library" }
if (-not $JavaHome) {
    $javacCommand = Get-Command javac.exe -ErrorAction Stop
    $JavaHome = Split-Path -Parent (Split-Path -Parent $javacCommand.Source)
}
$buildTools = Join-Path $Sdk "build-tools/$BuildToolsVersion"
$aapt2 = Join-Path $buildTools 'aapt2.exe'
$zipalign = Join-Path $buildTools 'zipalign.exe'
$apksigner = Join-Path $buildTools 'apksigner.bat'
$d8 = Join-Path $buildTools 'd8.bat'
$androidJar = Join-Path $Sdk "platforms/$Platform/android.jar"
$jar = Join-Path $JavaHome 'bin/jar.exe'
$keytool = Join-Path $JavaHome 'bin/keytool.exe'
$javac = Join-Path $JavaHome 'bin/javac.exe'
$readelf = Join-Path $Ndk 'toolchains/llvm/prebuilt/windows-x86_64/bin/llvm-readelf.exe'
$sysrootLibraries = Join-Path $Ndk 'toolchains/llvm/prebuilt/windows-x86_64/sysroot/usr/lib/aarch64-linux-android'
$manifest = Join-Path $PSScriptRoot 'game-shell/AndroidManifest.xml'
foreach ($tool in @($aapt2, $zipalign, $apksigner, $d8, $androidJar, $jar, $keytool, $javac, $readelf, $manifest)) {
    if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) { throw "Missing packaging input/tool: $tool" }
}
$Library = (Resolve-Path -LiteralPath $Library).Path
if ((Split-Path -Leaf $Library) -ne 'libadv_android.so') { throw 'Main library must be named libadv_android.so' }
$Output = [IO.Path]::GetFullPath($Output)
$Keystore = [IO.Path]::GetFullPath($Keystore)
New-Item -ItemType Directory -Force -Path $Output | Out-Null
[xml]$manifestDocument = Get-Content -LiteralPath $manifest -Raw
$androidNamespace = 'http://schemas.android.com/apk/res/android'
if ($manifestDocument.manifest.package -ne 'org.hitode.player') { throw 'Manifest package must be org.hitode.player' }
if ($manifestDocument.manifest.application.GetAttribute('extractNativeLibs', $androidNamespace) -ne 'false') { throw 'Manifest must set extractNativeLibs=false' }
$activities = @($manifestDocument.manifest.application.activity)
if (-not ($activities | Where-Object { $_.GetAttribute('name', $androidNamespace) -in @('.GameActivity', 'org.hitode.player.GameActivity') })) { throw 'Manifest must declare GameActivity' }
# Resolve the complete native dependency closure before compiling the launcher.
# Public API-29 linker stubs identify system libraries; private libraries must
# be packaged explicitly. libc++_shared is an app dependency when DT_NEEDED says so.
$nativeLibraries = @{}
$libraryQueue = [Collections.Generic.Queue[string]]::new()
$systemLibraries = [Collections.Generic.HashSet[string]]::new()
$searchPaths = @((Split-Path -Parent $Library)) + $AdditionalLibrarySearchPath
function Add-NativeLibrary([string]$Candidate) {
    $resolved = (Resolve-Path -LiteralPath $Candidate -ErrorAction Stop).Path
    $name = Split-Path -Leaf $resolved
    $digest = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($nativeLibraries.ContainsKey($name)) {
        if ($nativeLibraries[$name].sha256 -ne $digest) { throw "Different native libraries have the same filename: $name" }
        return
    }
    $nativeLibraries[$name] = [ordered]@{ name = $name; path = $resolved; sha256 = $digest; needed = @(); elf_load_segments = @() }
    $libraryQueue.Enqueue($resolved)
}
Add-NativeLibrary $Library
foreach ($extra in $AdditionalLibrary) { Add-NativeLibrary $extra }
while ($libraryQueue.Count) {
    $path = $libraryQueue.Dequeue()
    $name = Split-Path -Leaf $path
    $header = @(& $readelf -hW $path)
    if ($LASTEXITCODE -ne 0 -or ($header -join "`n") -notmatch 'Machine:\s+AArch64' -or ($header -join "`n") -notmatch 'Class:\s+ELF64') { throw "Native library is not an inspectable arm64 ELF: $path" }
    $headers = @(& $readelf -lW $path)
    if ($LASTEXITCODE -ne 0) { throw "ELF program-header inspection failed: $path" }
    $segments = @($headers | Where-Object { $_ -match '^\s*LOAD\s' })
    if (-not $segments.Count) { throw "Native library has no LOAD segments: $path" }
    foreach ($segment in $segments) {
        $alignment = ($segment.Trim() -split '\s+')[-1]
        if ([Convert]::ToInt64($alignment, 16) -lt 16384) { throw "ELF LOAD alignment below 16 KiB in $name : $segment" }
    }
    $dynamic = @(& $readelf -dW $path)
    if ($LASTEXITCODE -ne 0) { throw "ELF dependency inspection failed: $path" }
    $needed = @($dynamic | ForEach-Object { if ($_ -match '\(NEEDED\).*?\[([^\]]+)\]') { $Matches[1] } })
    $nativeLibraries[$name].needed = $needed
    $nativeLibraries[$name].elf_load_segments = $segments
    foreach ($dependency in $needed) {
        if ($dependency -notmatch '^[A-Za-z0-9_+.-]+$') { throw "Unsupported DT_NEEDED filename in $name : $dependency" }
        if ($nativeLibraries.ContainsKey($dependency)) { continue }
        $candidates = @($searchPaths | ForEach-Object { Join-Path $_ $dependency } | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -Unique)
        if ($dependency -eq 'libc++_shared.so') {
            $ndkCxx = Join-Path $sysrootLibraries $dependency
            if (-not $candidates.Count -and (Test-Path -LiteralPath $ndkCxx -PathType Leaf)) { $candidates = @($ndkCxx) }
        }
        if ($candidates.Count) {
            $candidateHashes = @($candidates | ForEach-Object { (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash } | Select-Object -Unique)
            if ($candidateHashes.Count -ne 1) { throw "Ambiguous native dependency $dependency; pass one matching library search path" }
            Add-NativeLibrary $candidates[0]
        } elseif ($dependency -ne 'libc++_shared.so' -and (Test-Path -LiteralPath (Join-Path $sysrootLibraries "29/$dependency") -PathType Leaf)) {
            $systemLibraries.Add($dependency) | Out-Null
        } else {
            throw "Required native dependency $dependency from $name is unavailable; supply -AdditionalLibrary or -AdditionalLibrarySearchPath"
        }
    }
}
$nativeResults = @($nativeLibraries.Values | Sort-Object name)
$sourceRoot = Join-Path $PSScriptRoot 'game-shell/java'
$sources = @(Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Filter '*.java' | ForEach-Object FullName)
if (-not $sources.Count) { throw 'No game shell Java sources' }
# Fresh staging avoids deleting or mixing previous build inputs and evidence.
$stage = Join-Path $Output ('stage-' + [Guid]::NewGuid().ToString('N'))
$classes = Join-Path $stage 'classes'
$dex = Join-Path $stage 'dex'
New-Item -ItemType Directory -Force -Path $classes, $dex | Out-Null
& $javac -encoding UTF-8 -source 8 -target 8 -classpath $androidJar -d $classes @sources
if ($LASTEXITCODE -ne 0) { throw 'Game shell javac failed' }
$classJar = Join-Path $stage 'launcher.jar'
& $jar cf $classJar -C $classes .
if ($LASTEXITCODE -ne 0) { throw 'Game shell launcher jar failed' }
$priorJavaHome = $env:JAVA_HOME
try {
    $env:JAVA_HOME = $JavaHome
    & $d8 --lib $androidJar --min-api 29 --output $dex $classJar
    if ($LASTEXITCODE -ne 0) { throw 'Game shell d8 failed' }
} finally { $env:JAVA_HOME = $priorJavaHome }
$unsigned = Join-Path $stage 'hitode-player-unsigned.apk'
$aligned = Join-Path $stage 'hitode-player-aligned.apk'
$signed = Join-Path $Output 'hitode-player.apk'
& $aapt2 link -o $unsigned -I $androidJar --manifest $manifest --min-sdk-version 29 --target-sdk-version 36
if ($LASTEXITCODE -ne 0) { throw 'Game shell aapt2 link failed' }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$dexFiles = @(Get-ChildItem -LiteralPath $dex -File -Filter 'classes*.dex')
if (-not $dexFiles.Count) { throw 'Game shell produced no DEX files' }
$archive = [IO.Compression.ZipFile]::Open($unsigned, [IO.Compression.ZipArchiveMode]::Update)
try {
    foreach ($dexFile in $dexFiles) { [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $dexFile.FullName, $dexFile.Name, [IO.Compression.CompressionLevel]::Optimal) | Out-Null }
    foreach ($native in $nativeResults) { [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $native.path, ('lib/arm64-v8a/' + $native.name), [IO.Compression.CompressionLevel]::NoCompression) | Out-Null }
} finally { $archive.Dispose() }
& $zipalign -P 16 -f 4 $unsigned $aligned
if ($LASTEXITCODE -ne 0) { throw 'Game shell 16 KiB zipalign failed' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Keystore) | Out-Null
if (-not (Test-Path -LiteralPath $Keystore -PathType Leaf)) {
    & $keytool -genkeypair -keystore $Keystore -alias androiddebugkey -storepass android -keypass android -dname 'CN=Android Debug,O=Android,C=US' -keyalg RSA -keysize 2048 -validity 10000
    if ($LASTEXITCODE -ne 0) { throw 'Game shell debug keytool failed' }
}
$priorJavaHome = $env:JAVA_HOME
try {
    $env:JAVA_HOME = $JavaHome
    & $apksigner sign --ks $Keystore --ks-key-alias androiddebugkey --ks-pass pass:android --key-pass pass:android --out $signed $aligned
    if ($LASTEXITCODE -ne 0) { throw 'Game shell signing failed' }
    & $apksigner verify --verbose --print-certs $signed
    if ($LASTEXITCODE -ne 0) { throw 'Game shell signature verification failed' }
} finally { $env:JAVA_HOME = $priorJavaHome }
& $zipalign -c -P 16 4 $signed
if ($LASTEXITCODE -ne 0) { throw 'Game shell signed APK alignment check failed' }
$archive = [IO.Compression.ZipFile]::OpenRead($signed)
try {
    foreach ($native in $nativeResults) {
        $entry = $archive.GetEntry('lib/arm64-v8a/' + $native.name)
        if ($null -eq $entry -or $entry.Length -ne $entry.CompressedLength) { throw "APK native library missing or compressed: $($native.name)" }
    }
} finally { $archive.Dispose() }
$nativeBuild = $null
if ($NativeBuildInfo) {
    $nativeBuild = Get-Content -LiteralPath $NativeBuildInfo -Raw | ConvertFrom-Json
    if ($nativeBuild.native_library_sha256 -ne $nativeLibraries['libadv_android.so'].sha256) { throw 'Native build provenance does not match the packaged library' }
}
$sourceInfo = @($sources | ForEach-Object { [ordered]@{ path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() } })
$metadata = [ordered]@{
    package = 'org.hitode.player'; activity = 'org.hitode.player.GameActivity'
    build_identifier = $BuildIdentifier; built_at_utc = [DateTime]::UtcNow.ToString('o')
    apk = $signed; apk_sha256 = (Get-FileHash -LiteralPath $signed -Algorithm SHA256).Hash.ToLowerInvariant()
    keystore = $Keystore; keystore_sha256 = (Get-FileHash -LiteralPath $Keystore -Algorithm SHA256).Hash.ToLowerInvariant()
    native_libraries = $nativeResults; native_build_info = $nativeBuild
    system_native_dependencies = @($systemLibraries | Sort-Object)
    java_sources = $sourceInfo; manifest_sha256 = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant()
    sdk = $Sdk; ndk = $Ndk; java_home = $JavaHome; build_tools = $BuildToolsVersion; platform = $Platform
    min_api = 29; target_api = 36; native_abi = 'arm64-v8a'; apk_native_page_alignment = 16384
}
$metadata | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath (Join-Path $Output 'build-info.json') -Encoding UTF8
Get-Item -LiteralPath $signed | Select-Object FullName, Length
