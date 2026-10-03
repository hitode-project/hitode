param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/winmm-tests'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'winmm_timers.c') "/Fo$OutputDirectory/winmm_timers.obj"
if ($LASTEXITCODE -ne 0) { throw 'WinMM lifecycle fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_winmm.exe" "$OutputDirectory/winmm_timers.obj" kernel32.lib winmm.lib
if ($LASTEXITCODE -ne 0) { throw 'WinMM lifecycle fixture link failed' }
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'winmm_budget.c') "/Fo$OutputDirectory/winmm_budget.obj"
if ($LASTEXITCODE -ne 0) { throw 'WinMM budget fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_winmm_budget.exe" "$OutputDirectory/winmm_budget.obj" kernel32.lib winmm.lib
if ($LASTEXITCODE -ne 0) { throw 'WinMM budget fixture link failed' }
