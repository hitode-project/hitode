param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/d3d9-shader-raster'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
$fxc = Join-Path $sdk "bin/$version/x64/fxc.exe"
$generated = Join-Path $OutputDirectory 'd3d9-shader-raster-generated.h'
Set-Content -LiteralPath $generated -Value '// Generated only from tools/fixtures/d3d9-shader-raster.hlsl.' -Encoding ascii
& $fxc /nologo /T vs_2_0 /E vertex_main /O3 /Fh $generated /Vn raster_vs (Join-Path $PSScriptRoot 'd3d9-shader-raster.hlsl') | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'raster shader compile failed' }
$clipHeader=Join-Path $OutputDirectory 'clip.h'
& $fxc /nologo /T vs_2_0 /E vertex_clip /O3 /Fh $clipHeader /Vn clip_vs (Join-Path $PSScriptRoot 'd3d9-shader-raster.hlsl') | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'clip shader compile failed' }
Get-Content -LiteralPath $clipHeader | Add-Content -LiteralPath $generated -Encoding ascii
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" "/I$OutputDirectory" (Join-Path $PSScriptRoot 'd3d9-shader-raster.c') "/Fo$OutputDirectory/d3d9-shader-raster.obj"
if ($LASTEXITCODE -ne 0) { throw 'D3D9 shader fixture compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_d3d9-shader-raster.exe" "$OutputDirectory/d3d9-shader-raster.obj" kernel32.lib user32.lib d3d9.lib
if ($LASTEXITCODE -ne 0) { throw 'D3D9 shader fixture link failed' }
