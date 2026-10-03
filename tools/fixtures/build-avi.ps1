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
