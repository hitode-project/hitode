param(
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures-gdi-text-paint'),
    [string]$LlvmBin='C:\Program Files\LLVM\bin',
    [string]$WindowsSdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'gdi_text_paint.c') "/Fo$OutputDirectory/paint.obj"
if($LASTEXITCODE -ne 0){throw 'GDI text paint compile failed'}
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_gdi_text_paint.exe" "$OutputDirectory/paint.obj" kernel32.lib gdi32.lib
if($LASTEXITCODE -ne 0){throw 'GDI text paint link failed'}
