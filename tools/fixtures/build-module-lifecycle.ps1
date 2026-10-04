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
Invoke-LifecycleChecked (Join-Path $LlvmBin 'clang.exe') @('--target=i686-pc-windows-msvc', '-c', (Join-Path $PSScriptRoot 'tls_array.s'), '-o', "$OutputDirectory/tls_array.obj")
$variants = @(
    @{Name='normal'; Id=1; Flags=@(); Libs=@()},
    @{Name='dep'; Id=2; Flags=@(); Libs=@()},
    @{Name='parent'; Id=3; Flags=@('/DWITH_DEPENDENCY'); Libs=@('lifecycle_dep.lib')},
    @{Name='fail'; Id=4; Flags=@('/DFAIL_ATTACH'); Libs=@()},
    @{Name='tls'; Id=5; Flags=@('/DWITH_TLS'); Libs=@("$OutputDirectory/tls_array.obj")},
    @{Name='slow'; Id=6; Flags=@('/DSLOW_ATTACH'); Libs=@()},
    @{Name='self'; Id=7; Flags=@('/DSELF_EXIT'); Libs=@('/export:SelfExit=_SelfExit@4')}
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
Write-Output "Built x86 module lifecycle fixtures in $OutputDirectory"
