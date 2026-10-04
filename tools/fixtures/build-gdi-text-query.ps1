param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-gdi-text-query'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
foreach ($variant in @('query','textout-stub')) {
    [string[]]$define = if ($variant -eq 'textout-stub') { @('/DPROBE_TEXTOUT') } else { @() }
    & (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc @define (Join-Path $PSScriptRoot 'gdi_text_query.c') "/Fo$OutputDirectory/$variant.obj"
    if ($LASTEXITCODE -ne 0) { throw 'GDI text query compile failed' }
    & (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_gdi_$variant.exe" "$OutputDirectory/$variant.obj" kernel32.lib gdi32.lib
    if ($LASTEXITCODE -ne 0) { throw 'GDI text query link failed' }
}
