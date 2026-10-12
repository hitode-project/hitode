<#
.SYNOPSIS
Build the authored x86 SRW lock and condition-variable contention oracle.
.DESCRIPTION
Input: srw.c and condition.c, LLVM and Windows SDK. Output: executable and object in
OutputDirectory. The fixture creates only worker threads and writes scalars.
#>
param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures/srw'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:/Program Files/LLVM/bin'; $sdk='C:/Program Files (x86)/Windows Kits/10'; $version='10.0.26100.0'
foreach($fixture in @('srw','condition')) {
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot "$fixture.c") "/Fo$OutputDirectory/$fixture.obj"
if($LASTEXITCODE-ne 0){throw 'SRW lock fixture compile failed'}
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_$fixture.exe" "$OutputDirectory/$fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'SRW lock fixture link failed'}
}
