param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/cpu-reference/native'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
$object = Join-Path $OutputDirectory 'segment-native.obj'
$executable = Join-Path $OutputDirectory 'segment-native.exe'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O1 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'segment-native.c') "/Fo$object"
if ($LASTEXITCODE -ne 0) { throw 'segment probe compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:console /entry:mainCRTStartup "/out:$executable" $object kernel32.lib
if ($LASTEXITCODE -ne 0) { throw 'segment probe link failed' }
$actual = (& $executable) -join "`n"
if ($LASTEXITCODE -ne 0) { throw "segment probe failed: $LASTEXITCODE" }
# Measured on current WoW64 (Intel Core Ultra 5 230F, Windows 11 26H2).
$expected = @'
sel es cs 0x0000002b 0x00000023
sel ss ds 0x0000002b 0x0000002b
sel fs gs 0x00000053 0x0000002b
push32 es 0xdead002b 0x00000004
push32 cs 0xdead0023 0x00000004
push32 ss 0xdead002b 0x00000004
push32 ds 0xdead002b 0x00000004
push32 fs 0xdead0053 0x00000004
push32 gs 0xdead002b 0x00000004
push16 es 0x002bbeef 0x00000002
push16 fs 0x0053beef 0x00000002
pop es 2b 0x0000002b 0x00000000
pop es 28 0x00000028 0x00000000
pop es 23 0x00000023 0x00000000
pop es 53 0x00000053 0x00000000
pop es 30 0x00000030 0x00000000
pop es 00 0x00000000 0x00000000
pop es 03 0x00000003 0x00000000
pop es 1234 0xffffffff 0xc0000005
pop es hi2b 0x0000002b 0x00000000
pop ds 23 0x00000023 0x00000000
pop ds 00 0x00000000 0x00000000
pop gs 00 0x00000000 0x00000000
pop gs 2b 0x0000002b 0x00000000
pop fs 53 0x00000053 0x00000000
gp info count 0x00000002 0x00000000
gp info 0x00000000 0xffffffff
pop ss 2b 0x0000002b 0x00000000
pop ss 00 0xffffffff 0xc0000005
pop ss 23 0xffffffff 0xc0000005
'@ -replace "`r", ''
if ($actual.Trim() -ne $expected.Trim()) { throw "segment probe differs:`n$actual" }
'Segment PUSH/POP matches the recorded WoW64 behavior.'
