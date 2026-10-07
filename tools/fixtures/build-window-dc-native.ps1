param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/window-dc-native'),
    [string]$LlvmBin = 'C:/Program Files/LLVM/bin',
    [string]$WindowsSdk = 'C:/Program Files (x86)/Windows Kits/10',
    [string]$SdkVersion = '10.0.26100.0'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$WindowsSdk/Include/$SdkVersion/um" "/I$WindowsSdk/Include/$SdkVersion/shared" "/I$WindowsSdk/Include/$SdkVersion/ucrt" (Join-Path $PSScriptRoot 'window-dc-native.c') "/Fo$OutputDirectory/window-dc-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'window DC native fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk/Lib/$SdkVersion/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/window-dc-native.exe" "$OutputDirectory/window-dc-native.obj" kernel32.lib user32.lib gdi32.lib
if ($LASTEXITCODE -ne 0) { throw 'window DC native fixture link failed' }
