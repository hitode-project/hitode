<#
.SYNOPSIS
Build the authored x86 window property oracle.
.DESCRIPTION
Input: window_property.c, LLVM and Windows SDK. Output: executable and object in
OutputDirectory. Native execution uses only message-only windows and emits scalars.
#>
param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/window-property/native'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:/Program Files/LLVM/bin'; $sdk='C:/Program Files (x86)/Windows Kits/10'; $version='10.0.26100.0'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'window_property.c') "/Fo$OutputDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'Window property fixture compile failed'}
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_window_property.exe" "$OutputDirectory/fixture.obj" kernel32.lib user32.lib
if($LASTEXITCODE-ne 0){throw 'Window property fixture link failed'}
function Hash([string]$path){
 $sha=[Security.Cryptography.SHA256]::Create()
 try { return [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($path))).Replace('-','').ToLowerInvariant() }
 finally { $sha.Dispose() }
}
$guestHash=Hash "$OutputDirectory/fixture_window_property.exe"
$sourceHash=Hash (Join-Path $PSScriptRoot 'window_property.c')
@("command: build-window-property.ps1 -OutputDirectory $OutputDirectory",'environment: LLVM clang-cl/lld-link; Windows SDK 10.0.26100.0; x86; no CRT',"guest_sha256: $guestHash","source_sha256: $sourceHash") | Set-Content -LiteralPath "$OutputDirectory/cmd.txt" -Encoding utf8
