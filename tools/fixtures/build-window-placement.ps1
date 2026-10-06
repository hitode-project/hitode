param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/window-placement'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:/Program Files/LLVM/bin'
$sdk = 'C:/Program Files (x86)/Windows Kits/10'
$version = '10.0.26100.0'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'window_placement.c') "/Fo$OutputDirectory/fixture.obj"
if ($LASTEXITCODE -ne 0) { throw 'Window placement fixture compile failed' }
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_window_placement.exe" "$OutputDirectory/fixture.obj" kernel32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'Window placement fixture link failed' }
