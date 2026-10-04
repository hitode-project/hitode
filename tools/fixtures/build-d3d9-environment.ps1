param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures'), [switch]$GuestVisible)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
$suffix=if($GuestVisible){'_visible'}else{''}
$define=@(if($GuestVisible){'/DGUEST_VISIBLE=1'})
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc @define "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'd3d9-environment.c') "/Fo$OutputDirectory/d3d9-environment$suffix.obj"
if ($LASTEXITCODE -ne 0) { throw 'D3D9 environment fixture compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_d3d9_environment$suffix.exe" "$OutputDirectory/d3d9-environment$suffix.obj" kernel32.lib user32.lib d3d9.lib
if ($LASTEXITCODE -ne 0) { throw 'D3D9 environment fixture link failed' }
