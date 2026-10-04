param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures-global-memory'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'global_memory.c') "/Fo$OutputDirectory/global.obj"
if($LASTEXITCODE -ne 0){throw 'global memory compile failed'}
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_global_memory.exe" "$OutputDirectory/global.obj" kernel32.lib
if($LASTEXITCODE -ne 0){throw 'global memory link failed'}
