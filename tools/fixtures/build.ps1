param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$compiler = Join-Path $LlvmBin 'clang-cl.exe'
$assembler = Join-Path $LlvmBin 'clang.exe'
$linker = Join-Path $LlvmBin 'lld-link.exe'
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
$common = @('/nologo', '/c', '/O2', '/GS-', '/Gs9999999', '/Zl', '/clang:--target=i686-pc-windows-msvc', "/I$PSScriptRoot")
Invoke-Checked $assembler @('--target=i686-pc-windows-msvc', '-c', (Join-Path $PSScriptRoot 'tls_array.s'), '-o', "$OutputDirectory/tls_array.obj")
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'probe.c'), "/Fo$OutputDirectory/probe.obj"))
Invoke-Checked $compiler ($common + @('/DDYNAMIC_PROBE', (Join-Path $PSScriptRoot 'probe.c'), "/Fo$OutputDirectory/dynamic.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'window.c'), "/Fo$OutputDirectory/window.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'wait.c'), "/Fo$OutputDirectory/wait.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'close.c'), "/Fo$OutputDirectory/close.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'control.c'), "/Fo$OutputDirectory/control.obj"))
Invoke-Checked $compiler ($common + @('/DCOMPUTE_WORKER', (Join-Path $PSScriptRoot 'control.c'), "/Fo$OutputDirectory/compute.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'audio.c'), "/Fo$OutputDirectory/audio.obj"))
Invoke-Checked $compiler ($common + @('/clang:-fno-unroll-loops', (Join-Path $PSScriptRoot 'slice.c'), "/Fo$OutputDirectory/slice.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'audio_unlock_probe.c'), "/Fo$OutputDirectory/audio_unlock_probe.obj"))
Invoke-Checked $compiler ($common + @((Join-Path $PSScriptRoot 'message_queue.c'), "/Fo$OutputDirectory/message_queue.obj"))
$base = @('/nologo', '/machine:x86', '/nodefaultlib', '/dynamicbase', '/base:0x400000', '/safeseh:no', '/timestamp:0', "/libpath:$WindowsSdk")
Invoke-Checked $linker ($base + @('/dll', '/entry:DllMain@12', "/def:$PSScriptRoot/probe.def", "/out:$OutputDirectory/fixture_probe.dll", "/implib:$OutputDirectory/fixture_probe.lib", "$OutputDirectory/probe.obj", "$OutputDirectory/tls_array.obj"))
Invoke-Checked $linker ($base + @('/dll', '/entry:DllMain@12', "/def:$PSScriptRoot/dynamic.def", "/out:$OutputDirectory/fixture_dynamic.dll", "/implib:$OutputDirectory/fixture_dynamic.lib", "$OutputDirectory/dynamic.obj"))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_window.exe", "$OutputDirectory/window.obj", "$OutputDirectory/fixture_probe.lib", 'kernel32.lib', 'user32.lib', 'gdi32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_wait.exe", "$OutputDirectory/wait.obj", 'kernel32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_close.exe", "$OutputDirectory/close.obj", 'kernel32.lib', 'user32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_control.exe", "$OutputDirectory/control.obj", 'kernel32.lib', 'user32.lib', 'gdi32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_compute.exe", "$OutputDirectory/compute.obj", 'kernel32.lib', 'user32.lib', 'gdi32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_audio.exe", "$OutputDirectory/audio.obj", 'kernel32.lib', 'dsound.lib'))
Invoke-Checked $linker ($base + @('/subsystem:windows', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_slice.exe", "$OutputDirectory/slice.obj", 'kernel32.lib'))
Invoke-Checked $linker ($base + @('/subsystem:console', '/entry:mainCRTStartup', "/out:$OutputDirectory/native_audio_unlock_probe.exe", "$OutputDirectory/audio_unlock_probe.obj", 'kernel32.lib', 'user32.lib', 'dsound.lib'))
Invoke-Checked $linker ($base + @('/subsystem:console', '/entry:mainCRTStartup', "/out:$OutputDirectory/fixture_message_queue.exe", "$OutputDirectory/message_queue.obj", 'kernel32.lib', 'user32.lib'))
Write-Output "Built x86 PE fixtures in $OutputDirectory"
