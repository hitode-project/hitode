param(
    [string]$Library = (Join-Path $PSScriptRoot '..\..\target\android-a0\aarch64-linux-android\release\libadv_android_probe.so'),
    [string]$Output = (Join-Path $PSScriptRoot '..\..\target\android-a0\apk-cpu-probe'),
    [string]$Sdk = $env:ANDROID_HOME,
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [string]$JavaHome = $env:JAVA_HOME,
    [string]$BuildToolsVersion = '36.0.0',
    [string]$Platform = 'android-36',
    [string]$BuildIdentifier = 'unlabelled'
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Library)) { throw "Missing Android CPU probe library: $Library" }
if (-not $JavaHome) {
    $javacCommand = Get-Command javac.exe -ErrorAction Stop
    $JavaHome = Split-Path -Parent (Split-Path -Parent $javacCommand.Source)
}
$buildTools = Join-Path $Sdk "build-tools\$BuildToolsVersion"
$aapt2 = Join-Path $buildTools 'aapt2.exe'
$zipalign = Join-Path $buildTools 'zipalign.exe'
$apksigner = Join-Path $buildTools 'apksigner.bat'
$d8 = Join-Path $buildTools 'd8.bat'
$androidJar = Join-Path $Sdk "platforms\$Platform\android.jar"
$jar = Join-Path $JavaHome 'bin\jar.exe'
$keytool = Join-Path $JavaHome 'bin\keytool.exe'
$javac = Join-Path $JavaHome 'bin\javac.exe'
$readelf = Join-Path $Ndk 'toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-readelf.exe'
foreach ($tool in @($aapt2, $zipalign, $apksigner, $d8, $androidJar, $jar, $keytool, $javac, $readelf)) {
    if (-not (Test-Path -LiteralPath $tool)) { throw "Missing packaging tool: $tool" }
}
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path -LiteralPath $Output).Path
$Library = (Resolve-Path -LiteralPath $Library).Path
# Each build starts with empty staging. No recursive cleanup of user paths.
$stage = Join-Path $Output ('stage-' + [Guid]::NewGuid().ToString('N'))
$classes = Join-Path $stage 'classes'
$dex = Join-Path $stage 'dex'
New-Item -ItemType Directory -Force -Path $classes, $dex | Out-Null
$sources = @(Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'cpu-probe\java') -Recurse -Filter '*.java' | ForEach-Object FullName)
& $javac -encoding UTF-8 -source 8 -target 8 -classpath $androidJar -d $classes @sources
if ($LASTEXITCODE -ne 0) { throw 'CPU probe javac failed' }
$classJar = Join-Path $stage 'launcher.jar'
& $jar cf $classJar -C $classes .
if ($LASTEXITCODE -ne 0) { throw 'CPU probe launcher jar failed' }
$priorJavaHome = $env:JAVA_HOME
try {
    $env:JAVA_HOME = $JavaHome
    & $d8 --lib $androidJar --min-api 29 --output $dex $classJar
    if ($LASTEXITCODE -ne 0) { throw 'CPU probe d8 failed' }
} finally { $env:JAVA_HOME = $priorJavaHome }
$unsigned = Join-Path $Output 'adv-cpu-probe-unsigned.apk'
$aligned = Join-Path $Output 'adv-cpu-probe-aligned.apk'
$signed = Join-Path $Output 'adv-cpu-probe.apk'
& $aapt2 link -o $unsigned -I $androidJar --manifest (Join-Path $PSScriptRoot 'cpu-probe\AndroidManifest.xml')
if ($LASTEXITCODE -ne 0) { throw 'CPU probe aapt2 link failed' }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::Open($unsigned, [IO.Compression.ZipArchiveMode]::Update)
try {
    [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $dex 'classes.dex'), 'classes.dex', [IO.Compression.CompressionLevel]::Optimal) | Out-Null
    # Required for extractNativeLibs=false and direct loading from the APK.
    [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $Library, 'lib/arm64-v8a/libadv_android_probe.so', [IO.Compression.CompressionLevel]::NoCompression) | Out-Null
} finally { $archive.Dispose() }
& $zipalign -P 16 -f 4 $unsigned $aligned
if ($LASTEXITCODE -ne 0) { throw 'CPU probe 16 KiB zipalign failed' }
$keystore = Join-Path $Output 'debug.keystore'
if (-not (Test-Path -LiteralPath $keystore)) {
    & $keytool -genkeypair -keystore $keystore -alias androiddebugkey -storepass android -keypass android -dname 'CN=Android Debug,O=Android,C=US' -keyalg RSA -keysize 2048 -validity 10000
    if ($LASTEXITCODE -ne 0) { throw 'CPU probe debug keytool failed' }
}
$priorJavaHome = $env:JAVA_HOME
try {
    $env:JAVA_HOME = $JavaHome
    & $apksigner sign --ks $keystore --ks-key-alias androiddebugkey --ks-pass pass:android --key-pass pass:android --out $signed $aligned
    if ($LASTEXITCODE -ne 0) { throw 'CPU probe signing failed' }
    & $apksigner verify --verbose $signed
    if ($LASTEXITCODE -ne 0) { throw 'CPU probe signature verification failed' }
} finally { $env:JAVA_HOME = $priorJavaHome }
& $zipalign -c -P 16 4 $signed
if ($LASTEXITCODE -ne 0) { throw 'CPU probe signed APK alignment check failed' }
$elfHeaders = & $readelf -lW $Library
if ($LASTEXITCODE -ne 0) { throw 'CPU probe ELF header inspection failed' }
$loadSegments = @($elfHeaders | Where-Object { $_ -match '^\s*LOAD\s' })
if ($loadSegments.Count -eq 0) { throw 'CPU probe ELF has no LOAD segments' }
foreach ($segment in $loadSegments) {
    $alignment = ($segment.Trim() -split '\s+')[-1]
    if ([Convert]::ToInt64($alignment, 16) -lt 16384) { throw "CPU probe ELF LOAD alignment below 16 KiB: $segment" }
}
$archive = [IO.Compression.ZipFile]::OpenRead($signed)
try {
    $nativeEntry = $archive.GetEntry('lib/arm64-v8a/libadv_android_probe.so')
    if ($nativeEntry.Length -ne $nativeEntry.CompressedLength) { throw 'CPU probe native library unexpectedly compressed' }
} finally { $archive.Dispose() }
$metadata = [ordered]@{
    package = 'org.advhost.cpuprobe'
    activity = 'org.advhost.cpuprobe.ProbeActivity'
    build_identifier = $BuildIdentifier
    apk = $signed
    apk_sha256 = (Get-FileHash -LiteralPath $signed -Algorithm SHA256).Hash.ToLowerInvariant()
    native_library = $Library
    native_library_sha256 = (Get-FileHash -LiteralPath $Library -Algorithm SHA256).Hash.ToLowerInvariant()
    sdk = $Sdk
    ndk = $Ndk
    java_home = $JavaHome
    build_tools = $BuildToolsVersion
    platform = $Platform
    min_api = 29
    native_abi = 'arm64-v8a'
    native_elf_load_segments = $loadSegments
    apk_native_page_alignment = 16384
    private_report = 'files/cpu-probe.json'
}
$metadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $Output 'build-info.json') -Encoding UTF8
Get-Item -LiteralPath $signed | Select-Object FullName, Length
