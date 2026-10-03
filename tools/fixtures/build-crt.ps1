param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-crt'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
foreach ($taskVariant in @('native', 'aliases')) {
    [string[]]$taskDefines = if ($taskVariant -eq 'aliases') { @('/DHOST_CRT_ALIASES') } else { @() }
    & (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc @taskDefines "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'crt_init.c') "/Fo$OutputDirectory/crt_init_$taskVariant.obj"
    if ($LASTEXITCODE -ne 0) { throw 'CRT fixture compile failed' }
    & (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_crt_$taskVariant.exe" "$OutputDirectory/crt_init_$taskVariant.obj" kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw 'CRT fixture link failed' }
}
Write-Output "Built CRT fixtures in $OutputDirectory"
