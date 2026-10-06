param(
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures/d3d9-shaders-effect'),
    [string]$LegacySdkInclude
)
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
if(-not $LegacySdkInclude){
    $cache=Join-Path $OutputDirectory 'legacy-sdk'
    $LegacySdkInclude=Join-Path $cache 'build/native/include'
    if(-not (Test-Path -LiteralPath (Join-Path $LegacySdkInclude 'd3dx9effect.h'))){
        $archive=Join-Path $OutputDirectory 'd3dx-sdk.zip'
        Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/microsoft.dxsdk.d3dx/9.29.952.8/microsoft.dxsdk.d3dx.9.29.952.8.nupkg' -OutFile $archive
        if((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash-ne 'EAD0906AE8A26C18A7525DA7490127A2110F7C58F18293738283E30E97C6EA4B'){throw 'legacy SDK package hash changed'}
        Expand-Archive -LiteralPath $archive -DestinationPath $cache -Force
    }
}
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $sdk "bin/$version/x64/fxc.exe") /nologo /T fx_2_0 /O3 /Fh (Join-Path $OutputDirectory 'd3d9-shaders-effect-generated.h') /Vn authored_effect /Fo (Join-Path $OutputDirectory 'authored_effect.fxo') (Join-Path $PSScriptRoot 'd3d9-shaders-effect.fx') | Out-Null
if($LASTEXITCODE-ne 0){throw 'authored Effect compilation failed'}
& (Join-Path $sdk "bin/$version/x64/fxc.exe") /nologo /T fx_2_0 /O3 /Fh (Join-Path $OutputDirectory 'd3d9-shaders-effect-parameters-generated.h') /Vn authored_parameters /Fo (Join-Path $OutputDirectory 'authored_parameters.fxo') (Join-Path $PSScriptRoot 'd3d9-shaders-effect-parameters.fx') | Out-Null
if($LASTEXITCODE-ne 0){throw 'authored Effect parameter compilation failed'}
foreach($fixtureName in @('d3d9-shaders-effect','d3d9-shaders-effect-input','d3d9-shaders-effect-parameters')){
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" "/I$LegacySdkInclude" "/I$OutputDirectory" (Join-Path $PSScriptRoot "$fixtureName.c") "/Fo$OutputDirectory/$fixtureName.obj"
    if($LASTEXITCODE-ne 0){throw "Effect fixture compile failed: $fixtureName"}
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_$fixtureName.exe" "$OutputDirectory/$fixtureName.obj" kernel32.lib user32.lib d3d9.lib
    if($LASTEXITCODE-ne 0){throw "Effect fixture link failed: $fixtureName"}
}
