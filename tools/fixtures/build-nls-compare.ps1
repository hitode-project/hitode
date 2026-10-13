<#
.SYNOPSIS
Build the authored x86 NLS comparison oracle.
.DESCRIPTION
Input: nls-compare.c, LLVM and Windows SDK. Output: executable and object in
OutputDirectory. Native execution compares authored strings; output is scalar.
#>
param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/nls-compare/native'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:/Program Files/LLVM/bin'; $sdk='C:/Program Files (x86)/Windows Kits/10'; $version='10.0.26100.0'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nls-compare.c') "/Fo$OutputDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'NLS comparison fixture compile failed'}
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_nls_compare.exe" "$OutputDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'NLS comparison fixture link failed'}
function Hash([string]$path){
 $sha=[Security.Cryptography.SHA256]::Create()
 try { return [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($path))).Replace('-','').ToLowerInvariant() }
 finally { $sha.Dispose() }
}
$guestHash=Hash "$OutputDirectory/fixture_nls_compare.exe"
$sourceHash=Hash (Join-Path $PSScriptRoot 'nls-compare.c')
@("command: build-nls-compare.ps1 -OutputDirectory $OutputDirectory",'environment: LLVM clang-cl/lld-link; Windows SDK 10.0.26100.0; x86; no CRT',"guest_sha256: $guestHash","source_sha256: $sourceHash") | Set-Content -LiteralPath "$OutputDirectory/cmd.txt" -Encoding utf8
