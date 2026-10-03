param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-ole-memory'))
$ErrorActionPreference='Stop'
$OutputDirectory=[System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'ole_memory.c') "/Fo$OutputDirectory/ole_memory.obj"
if ($LASTEXITCODE -ne 0) { throw 'OLE memory fixture compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_ole_memory.exe" "$OutputDirectory/ole_memory.obj" kernel32.lib ole32.lib oleaut32.lib
if ($LASTEXITCODE -ne 0) { throw 'OLE memory fixture link failed' }
