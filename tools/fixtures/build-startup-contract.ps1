param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/startup-contract'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10'
$version = '10.0.26100.0'
foreach ($variant in @('startup_contract', 'virtual_startup_contract')) {
    $define = if ($variant.StartsWith('virtual')) { @('/DVIRTUAL_PROFILE=1') } else { @() }
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /utf-8 /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc $define "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" (Join-Path $PSScriptRoot 'startup_contract.c') "/Fo$OutputDirectory/$variant.obj"
    if ($LASTEXITCODE -ne 0) { throw 'Startup fixture compile failed' }
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 /manifest:embed "/manifestuac:level='asInvoker' uiAccess='false'" "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_$variant.exe" "$OutputDirectory/$variant.obj" kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw 'Startup fixture link failed' }
}
