param([string]$Output = (Join-Path $PSScriptRoot '../../target/android-shell/fixture'))
$ErrorActionPreference = 'Stop'
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$fixtureRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../fixtures'))
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$fixtureRoot" (Join-Path $fixtureRoot 'android_shell.c') "/Fo$Output/shell-probe.obj"
if ($LASTEXITCODE -ne 0) { throw 'Android shell fixture compilation failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:windows /entry:mainCRTStartup "/out:$Output/shell-probe.exe" "$Output/shell-probe.obj" kernel32.lib user32.lib gdi32.lib dsound.lib advapi32.lib
if ($LASTEXITCODE -ne 0) { throw 'Android shell fixture link failed' }
Get-FileHash -LiteralPath (Join-Path $Output 'shell-probe.exe') -Algorithm SHA256
