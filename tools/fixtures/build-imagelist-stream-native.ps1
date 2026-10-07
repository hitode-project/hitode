param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/investigations/imagelist-stream/native'),
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
$source = Join-Path $PSScriptRoot 'imagelist-stream-native.c'
$executable = Join-Path $OutputDirectory 'imagelist-stream-native.exe'
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$MsvcRoot/include" "/I$include/um" "/I$include/shared" "/I$include/ucrt" $source "/Fo$OutputDirectory/imagelist-stream-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'ImageList stream native compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$library" "/libpath:$MsvcRoot/lib/x86" /subsystem:console /entry:mainCRTStartup "/out:$executable" "$OutputDirectory/imagelist-stream-native.obj" kernel32.lib ole32.lib comctl32.lib gdi32.lib user32.lib vcruntime.lib
if ($LASTEXITCODE -ne 0) { throw 'ImageList stream native link failed' }
@(
    "Command: $executable",
    'Environment: native Windows x86, no guest/sample files, inherited process environment',
    "Executable SHA-256: $((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash)",
    "Authored source SHA-256: $((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash)"
) | Set-Content -LiteralPath (Join-Path $OutputDirectory 'cmd.txt')
$process = Start-Process -FilePath $executable -WorkingDirectory $OutputDirectory -WindowStyle Hidden -RedirectStandardOutput (Join-Path $OutputDirectory 'native.log') -RedirectStandardError (Join-Path $OutputDirectory 'native.stderr.log') -PassThru
if (-not $process.WaitForExit(5000)) {
    Stop-Process -Id $process.Id
    throw 'ImageList stream native exceeded five-second deadline'
}
$process.Refresh()
if ($process.ExitCode -ne 0) { throw "ImageList stream native failed: $($process.ExitCode)" }
Get-Content -LiteralPath (Join-Path $OutputDirectory 'native.log')
