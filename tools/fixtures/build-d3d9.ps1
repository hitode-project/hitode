param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures'),
    [ValidateSet('d3d9','d3d9-capabilities','d3d9-present','d3d9-buffers','d3d9-targets','d3d9-alpha','d3d9-creation','d3d9-depth','d3d9-clear','d3d9-mips','d3d9-version','d3d9-lockable','d3dx9-surface-save','d3dx9-texture-create','d3dx9-math2')][string]$Name = 'd3d9'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot "$Name.c") "/Fo$OutputDirectory/$Name.obj"
if ($LASTEXITCODE -ne 0) { throw 'D3D9 fixture compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_$Name.exe" "$OutputDirectory/$Name.obj" kernel32.lib user32.lib d3d9.lib
if ($LASTEXITCODE -ne 0) { throw 'D3D9 fixture link failed' }
