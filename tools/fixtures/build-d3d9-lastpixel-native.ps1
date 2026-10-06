param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/d3d9-lastpixel-native'),
    [string]$SdkDirectory = (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'),
    [string]$SdkVersion = '10.0.26100.0',
    [string]$LlvmDirectory = (Join-Path $env:ProgramFiles 'LLVM/bin'),
    [switch]$Run
)
$ErrorActionPreference = 'Stop'
$nativeDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$nativeSource = Join-Path $PSScriptRoot 'd3d9-lastpixel-native.c'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
& (Join-Path $LlvmDirectory 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$SdkDirectory/Include/$SdkVersion/um" "/I$SdkDirectory/Include/$SdkVersion/shared" "/I$SdkDirectory/Include/$SdkVersion/ucrt" $nativeSource "/Fo$nativeDirectory/d3d9-lastpixel-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'Native LASTPIXEL compile failed' }
& (Join-Path $LlvmDirectory 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$SdkDirectory/Lib/$SdkVersion/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$nativeDirectory/d3d9-lastpixel-native.exe" "$nativeDirectory/d3d9-lastpixel-native.obj" kernel32.lib user32.lib d3d9.lib
if ($LASTEXITCODE -ne 0) { throw 'Native LASTPIXEL link failed' }
$compilerVersion = & (Join-Path $LlvmDirectory 'clang-cl.exe') --version
@{
    utc = [DateTime]::UtcNow.ToString('o')
    os = [Environment]::OSVersion.VersionString
    architecture = 'x86'
    sdk = $SdkVersion
    compiler = $compilerVersion
    sourceSha256 = (Get-FileHash -LiteralPath $nativeSource -Algorithm SHA256).Hash
    exeSha256 = (Get-FileHash -LiteralPath "$nativeDirectory/d3d9-lastpixel-native.exe" -Algorithm SHA256).Hash
} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$nativeDirectory/environment.json" -Encoding utf8
if ($Run) {
    $nativeProcess = Start-Process -FilePath "$nativeDirectory/d3d9-lastpixel-native.exe" -WorkingDirectory $nativeDirectory -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput "$nativeDirectory/native.jsonl" -RedirectStandardError "$nativeDirectory/native.stderr.txt"
    if ($nativeProcess.ExitCode -ne 0) { throw "Native LASTPIXEL oracle exited $($nativeProcess.ExitCode)" }
    $nativeRows = @(Get-Content -LiteralPath "$nativeDirectory/native.jsonl" | ForEach-Object { $_ | ConvertFrom-Json })
    $nativeCases = @($nativeRows | Where-Object op -eq 'case')
    if ($nativeRows[-1].op -ne 'complete' -or $nativeRows[-1].result -ne 1 -or $nativeCases.Count -ne $nativeRows[-1].case_count) { throw 'Native LASTPIXEL output is incomplete' }
    foreach ($nativeCase in $nativeCases) {
        if (-not $nativeCase.all_hresult_s_ok -or $nativeCase.actual_lastpixel -ne $nativeCase.lastpixel) { throw 'Native LASTPIXEL state/HRESULT validation failed' }
    }
    $referenceAvailability = @($nativeRows | Where-Object op -eq 'reference_availability')
    Write-Output "Native LASTPIXEL oracle: $($nativeCases.Count) case rows, REF create HRESULT $($referenceAvailability[0].create_hr); $nativeDirectory/native.jsonl"
}
