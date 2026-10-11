<#
.SYNOPSIS
Build synthetic x86 MMIO and ACM fixtures for native/guest differential tests.
.DESCRIPTION
Input: authored C sources and installed LLVM/Windows SDK. Output: executables
and object files in OutputDirectory. No game resources are read.
#>
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-mmio'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'mmio_riff.c') "/Fo$OutputDirectory/mmio_riff.obj"
if ($LASTEXITCODE -ne 0) { throw 'MMIO fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_mmio.exe" "$OutputDirectory/mmio_riff.obj" kernel32.lib winmm.lib
if ($LASTEXITCODE -ne 0) { throw 'MMIO fixture link failed' }
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'acm_codec.c') "/Fo$OutputDirectory/acm_codec.obj"
if ($LASTEXITCODE -ne 0) { throw 'ACM fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_acm_codec.exe" "$OutputDirectory/acm_codec.obj" kernel32.lib msacm32.lib
if ($LASTEXITCODE -ne 0) { throw 'ACM fixture link failed' }
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc /DHOST_BOUNDARIES "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'acm_codec.c') "/Fo$OutputDirectory/acm_codec_boundaries.obj"
if ($LASTEXITCODE -ne 0) { throw 'ACM boundary fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_acm_codec_boundaries.exe" "$OutputDirectory/acm_codec_boundaries.obj" kernel32.lib msacm32.lib
if ($LASTEXITCODE -ne 0) { throw 'ACM boundary fixture link failed' }
Write-Output "Built MMIO fixture in $OutputDirectory"
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'gdi_region.c') "/Fo$OutputDirectory/gdi_region.obj"
if ($LASTEXITCODE -ne 0) { throw 'GDI region fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_gdi_region.exe" "$OutputDirectory/gdi_region.obj" kernel32.lib gdi32.lib
if ($LASTEXITCODE -ne 0) { throw 'GDI region fixture link failed' }
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'mci_video.c') "/Fo$OutputDirectory/mci_video.obj"
if ($LASTEXITCODE -ne 0) { throw 'MCI video fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_mci_video.exe" "$OutputDirectory/mci_video.obj" kernel32.lib user32.lib winmm.lib
if ($LASTEXITCODE -ne 0) { throw 'MCI video fixture link failed' }
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'acm_function.c') "/Fo$OutputDirectory/acm_function.obj"
if ($LASTEXITCODE -ne 0) { throw 'ACM function fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_acm_function.exe" "$OutputDirectory/acm_function.obj" kernel32.lib msacm32.lib winmm.lib
if ($LASTEXITCODE -ne 0) { throw 'ACM function fixture link failed' }
