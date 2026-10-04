param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures-clipboard-guest'))
# Build only. Never start this executable on the host's real clipboard.
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'clipboard_guest.c') "/Fo$OutputDirectory/clipboard.obj"
if($LASTEXITCODE -ne 0){throw 'guest-only clipboard compile failed'}
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_clipboard_guest.exe" "$OutputDirectory/clipboard.obj" kernel32.lib user32.lib
if($LASTEXITCODE -ne 0){throw 'guest-only clipboard link failed'}
