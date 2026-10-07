param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/tsf-lifecycle-native'),
    [string]$LlvmBin = 'C:/Program Files/LLVM/bin',
    [string]$WindowsSdkRoot = 'C:/Program Files (x86)/Windows Kits/10',
    [string]$SdkVersion = '10.0.26100.0',
    [string]$MsvcRoot = 'C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$include = Join-Path $WindowsSdkRoot "Include/$SdkVersion"
$library = Join-Path $WindowsSdkRoot "Lib/$SdkVersion/um/x86"
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:--target=i686-pc-windows-msvc "/I$MsvcRoot/include" "/I$include/um" "/I$include/shared" "/I$include/ucrt" (Join-Path $PSScriptRoot 'tsf-lifecycle-native.c') "/Fo$OutputDirectory/tsf-lifecycle-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'TSF lifecycle native compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$library" "/libpath:$MsvcRoot/lib/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/tsf-lifecycle-native.exe" "$OutputDirectory/tsf-lifecycle-native.obj" kernel32.lib ole32.lib uuid.lib vcruntime.lib
if ($LASTEXITCODE -ne 0) { throw 'TSF lifecycle native link failed' }
$process = Start-Process -FilePath (Join-Path $OutputDirectory 'tsf-lifecycle-native.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -RedirectStandardOutput (Join-Path $OutputDirectory 'tsf-lifecycle-native.log') -RedirectStandardError (Join-Path $OutputDirectory 'tsf-lifecycle-native.stderr.log') -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "TSF lifecycle native failed: $($process.ExitCode)" }
Get-Content -LiteralPath (Join-Path $OutputDirectory 'tsf-lifecycle-native.log')
