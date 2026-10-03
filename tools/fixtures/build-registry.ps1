param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-registry'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'registry_groups.c') "/Fo$OutputDirectory/registry_groups.obj"
if ($LASTEXITCODE -ne 0) { throw 'Registry fixture compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_registry_groups.exe" "$OutputDirectory/registry_groups.obj" kernel32.lib advapi32.lib
if ($LASTEXITCODE -ne 0) { throw 'Registry fixture link failed' }
