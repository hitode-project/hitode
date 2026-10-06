param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/d3d9-shaders'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
$fxc = Join-Path $sdk "bin/$version/x64/fxc.exe"
$generated = Join-Path $OutputDirectory 'd3d9-shaders-generated.h'
Set-Content -LiteralPath $generated -Value '// Generated only from tools/fixtures/d3d9-shaders.hlsl.' -Encoding ascii
foreach ($shader in @(
    @{Name='vs20'; Entry='vertex_main'; Profile='vs_2_0'},
    @{Name='ps20_color'; Entry='pixel_color'; Profile='ps_2_0'},
    @{Name='ps20_texture'; Entry='pixel_texture'; Profile='ps_2_0'},
    @{Name='vs30'; Entry='vertex_main'; Profile='vs_3_0'},
    @{Name='ps30_flow'; Entry='pixel_flow'; Profile='ps_3_0'}
)) {
    $header = Join-Path $OutputDirectory ($shader.Name + '.h')
    & $fxc /nologo /T $shader.Profile /E $shader.Entry /O3 /Gfp /Fh $header /Vn $shader.Name /Fo (Join-Path $OutputDirectory ($shader.Name + '.bin')) (Join-Path $PSScriptRoot 'd3d9-shaders.hlsl') | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "fxc failed for $($shader.Name)" }
    Get-Content -LiteralPath $header | Add-Content -LiteralPath $generated -Encoding ascii
}
foreach($fixtureName in @('d3d9-shaders','d3d9-shaders-semantics')){
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" "/I$OutputDirectory" (Join-Path $PSScriptRoot "$fixtureName.c") "/Fo$OutputDirectory/$fixtureName.obj"
    if ($LASTEXITCODE -ne 0) { throw "D3D9 shader fixture compile failed: $fixtureName" }
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_$fixtureName.exe" "$OutputDirectory/$fixtureName.obj" kernel32.lib user32.lib d3d9.lib
    if ($LASTEXITCODE -ne 0) { throw "D3D9 shader fixture link failed: $fixtureName" }
}
