<#
Build authored x86 AVI lifetime and native-only open-error probes with LLVM and the Windows SDK.
Inputs: avi_lifecycle.c and synthetic missing, empty, corrupt, and truncated files.
Lifecycle checks compare balanced Init/Exit calls and stack deltas, excluding undefined EAX.
Run from the repository root: ./tools/fixtures/build-avi.ps1
Run fixture_avi_lifecycle.exe or fixture_avi_errors.exe from the output directory.
Each executable writes avi-report.bin in its working directory.
Default binaries: target/avi-tests. Neither probe uses game data, a window, or audio.
Guest comparison: cargo test -p adv-runtime --test avi
#>
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/avi-tests'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
foreach ($Kind in @('lifecycle','errors')) {
    [string[]]$Defines = if ($Kind -eq 'errors') { @('/DAVI_OPEN_ERRORS') } else { @() }
    & (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc @Defines "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'avi_lifecycle.c') "/Fo$OutputDirectory/avi_$Kind.obj"
    if ($LASTEXITCODE -ne 0) { throw "AVI $Kind fixture compile failed" }
    & (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_avi_$Kind.exe" "$OutputDirectory/avi_$Kind.obj" kernel32.lib vfw32.lib
    if ($LASTEXITCODE -ne 0) { throw "AVI $Kind fixture link failed" }
}
