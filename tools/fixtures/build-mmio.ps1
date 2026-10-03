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
