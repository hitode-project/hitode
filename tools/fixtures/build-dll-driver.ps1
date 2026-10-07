<#
.SYNOPSIS
Build the no-CRT x86 JSON DLL driver and repository-owned ABI probe with LLVM.
.EXAMPLE
./tools/fixtures/build-dll-driver.ps1 -OutputDirectory target/fixtures-dll-driver
.INPUTS
dll-driver.c, dll-driver-probe.c, their DEF and example JSON; LLVM and SDK import libraries.
.OUTPUTS
EXE, probe DLL, objects/import library and dll-driver.json in OutputDirectory.
Run tools/performance/dll_exports.py to collect native/interpreter/JIT statistics.
#>
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-dll-driver'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
function Invoke-DriverChecked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
$compiler = Join-Path $LlvmBin 'clang-cl.exe'
$linker = Join-Path $LlvmBin 'lld-link.exe'
$compile = @('/nologo', '/c', '/O2', '/GS-', '/Gs9999999', '/Zl', '/W4', '/WX', '/clang:-fno-builtin', '/clang:--target=i686-pc-windows-msvc', "/I$PSScriptRoot")
$link = @('/nologo', '/machine:x86', '/nodefaultlib', '/dynamicbase', '/safeseh:no', '/timestamp:0', '/manifest:embed', "/manifestuac:level='asInvoker' uiAccess='false'", "/libpath:$WindowsSdk")
Invoke-DriverChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'dll-driver.c'), "/Fo$OutputDirectory/dll-driver.obj"))
Invoke-DriverChecked $linker ($link + @('/subsystem:console', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_dll_driver.exe", "$OutputDirectory/dll-driver.obj", 'kernel32.lib'))
Invoke-DriverChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'dll-driver-probe.c'), "/Fo$OutputDirectory/dll-driver-probe.obj"))
Invoke-DriverChecked $linker ($link + @('/dll', '/entry:DllMain@12', "/def:$PSScriptRoot/dll-driver-probe.def", "/out:$OutputDirectory/fixture_export_probe.dll", "/implib:$OutputDirectory/fixture_export_probe.lib", "$OutputDirectory/dll-driver-probe.obj"))
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'dll-driver-example.json') -Destination (Join-Path $OutputDirectory 'dll-driver.json')
Write-Output 'Built 1 x86 DLL export driver and 1 probe DLL'
