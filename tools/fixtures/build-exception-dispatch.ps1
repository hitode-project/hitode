<#
.SYNOPSIS
Build authored x86 exception-dispatch, heap or native-chance fixtures without a CRT.
.DESCRIPTION
Input: checked-in C fixture, local LLVM and Windows SDK. OutputDirectory receives
the EXE and object. Select Fixture=heap-generate-exceptions for the heap oracle.
Fixture=native-exception-chance compares native first/second-chance termination;
it is verified by the adv-runtime exception_chance integration test.
.EXAMPLE
pwsh tools/fixtures/build-exception-dispatch.ps1 -Fixture heap-generate-exceptions -OutputDirectory target/scratch/heap-oracle
#>
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/exception-dispatch'),
    [ValidateSet('dispatch', 'heap-generate-exceptions', 'native-exception-chance')]
    [string]$Fixture = 'dispatch'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
foreach ($variant in @($Fixture)) {
    $defines = @()
    $source = if ($variant -eq 'dispatch') { 'exception_dispatch.c' } else { "$variant.c" }
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc @defines "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot $source) "/Fo$OutputDirectory/exception_$variant.obj"
    if ($LASTEXITCODE -ne 0) { throw 'Exception fixture compile failed' }
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 /manifest:embed "/manifestuac:level='asInvoker' uiAccess='false'" "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_exception_$variant.exe" "$OutputDirectory/exception_$variant.obj" kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw 'Exception fixture link failed' }
}
