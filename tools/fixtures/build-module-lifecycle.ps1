<#
.SYNOPSIS
Build authored x86 module and thread lifecycle fixtures.
.DESCRIPTION
Input: the adjacent synthetic sources and installed LLVM/Windows SDK.
Output: executables, DLLs and objects in OutputDirectory; no sample inputs.
#>
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-module-lifecycle'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
function Invoke-LifecycleChecked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
$compiler = Join-Path $LlvmBin 'clang-cl.exe'
$linker = Join-Path $LlvmBin 'lld-link.exe'
$compile = @('/nologo', '/c', '/O2', '/GS-', '/Gs9999999', '/Zl', '/clang:-fno-builtin', '/clang:--target=i686-pc-windows-msvc', "/I$PSScriptRoot")
$link = @('/nologo', '/machine:x86', '/nodefaultlib', '/dynamicbase', '/safeseh:no', '/timestamp:0', '/manifest:embed', "/manifestuac:level='asInvoker' uiAccess='false'", "/libpath:$WindowsSdk", "/libpath:$OutputDirectory")
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'thread_create_flags.c'), "/Fo$OutputDirectory/thread_create_flags.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_thread_create_flags.exe", "$OutputDirectory/thread_create_flags.obj", 'kernel32.lib'))
Invoke-LifecycleChecked (Join-Path $LlvmBin 'clang.exe') @('--target=i686-pc-windows-msvc', '-c', (Join-Path $PSScriptRoot 'tls_array.s'), '-o', "$OutputDirectory/tls_array.obj")
$variants = @(
    @{Name='normal'; Id=1; Flags=@(); Libs=@()},
    @{Name='dep'; Id=2; Flags=@(); Libs=@()},
    @{Name='parent'; Id=3; Flags=@('/DWITH_DEPENDENCY'); Libs=@('lifecycle_dep.lib')},
    @{Name='fail'; Id=4; Flags=@('/DFAIL_ATTACH'); Libs=@()},
    @{Name='tls'; Id=5; Flags=@('/DWITH_TLS'); Libs=@("$OutputDirectory/tls_array.obj")},
    @{Name='slow'; Id=6; Flags=@('/DSLOW_ATTACH'); Libs=@()},
    @{Name='self'; Id=7; Flags=@('/DSELF_EXIT'); Libs=@('/export:SelfExit=_SelfExit@4')},
    @{Name='forward'; Id=8; Flags=@(); Libs=@('/export:Later=lifecycle_dep.Dependency', '/export:Failed=lifecycle_fail.Dependency', '/export:FailedTree=lifecycle_failtree.LinkedValue', '/export:Absent=kernel32.AdvFixtureUnimplemented', '/export:AbsentGuest=lifecycle_dep.DoesNotExist', '/export:Cycle=lifecycle_forward.Cycle')},
    @{Name='failtree'; Id=10; Flags=@('/DWITH_TLS_DEPENDENCY', '/DFAIL_ATTACH'); Libs=@('lifecycle_tls.lib')},
    @{Name='badforward'; Id=9; Flags=@('/DWITH_FORWARD_IMPORT'); Libs=@('lifecycle_forward.lib')}
)
foreach ($variant in $variants) {
    $name = 'lifecycle_' + $variant.Name
    Invoke-LifecycleChecked $compiler ($compile + @("/DMODULE_ID=$($variant.Id)") + $variant.Flags + @((Join-Path $PSScriptRoot 'module_lifecycle_dll.c'), "/Fo$OutputDirectory/$name.obj"))
    Invoke-LifecycleChecked $linker ($link + @('/dll', '/base:0x10000000', '/entry:DllMain@12', "/out:$OutputDirectory/$name.dll", "/implib:$OutputDirectory/$name.lib", "$OutputDirectory/$name.obj", 'kernel32.lib') + $variant.Libs)
}
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'module_lifecycle.c'), "/Fo$OutputDirectory/module_lifecycle.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_module_lifecycle.exe", "$OutputDirectory/module_lifecycle.obj", 'kernel32.lib'))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'module_notifications.c'), "/Fo$OutputDirectory/module_notifications.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_module_notifications.exe", "$OutputDirectory/module_notifications.obj", 'kernel32.lib'))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'startup_thread_dll.c'), "/Fo$OutputDirectory/startup_thread.obj"))
Invoke-LifecycleChecked $linker ($link + @('/dll', '/base:0x18000000', '/entry:DllMain@12', "/out:$OutputDirectory/lifecycle_startup.dll", "/implib:$OutputDirectory/lifecycle_startup.lib", "$OutputDirectory/startup_thread.obj", 'kernel32.lib'))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'thread_exit.c'), "/Fo$OutputDirectory/thread_exit.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_thread_exit.exe", "$OutputDirectory/thread_exit.obj", 'lifecycle_startup.lib', 'kernel32.lib'))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'module_self_exit.c'), "/Fo$OutputDirectory/module_self_exit.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_module_self_exit.exe", "$OutputDirectory/module_self_exit.obj", 'kernel32.lib'))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'module_forwarder.c'), "/Fo$OutputDirectory/module_forwarder.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_module_forwarder.exe", "$OutputDirectory/module_forwarder.obj", 'kernel32.lib'))
Invoke-LifecycleChecked (Join-Path $LlvmBin 'llvm-rc.exe') @('/nologo', "/fo$OutputDirectory/resource_handles.res", (Join-Path $PSScriptRoot 'resource_handles.rc'))
Invoke-LifecycleChecked $linker ($link + @('/dll', '/noentry', '/base:0x18000000', "/out:$OutputDirectory/lifecycle_resources.dll", "$OutputDirectory/resource_handles.res"))
Invoke-LifecycleChecked $compiler ($compile + @((Join-Path $PSScriptRoot 'resource_handles.c'), "/Fo$OutputDirectory/resource_handles.obj"))
Invoke-LifecycleChecked $linker ($link + @('/subsystem:console', '/base:0x400000', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_resource_handles.exe", "$OutputDirectory/resource_handles.obj", 'kernel32.lib'))
Write-Output "Built x86 module lifecycle fixtures in $OutputDirectory"
