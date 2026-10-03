param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-file-mapping'))
$ErrorActionPreference='Stop'
$OutputDirectory=[System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$taskLlvm='C:\Program Files\LLVM\bin'
$taskSdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& (Join-Path $taskLlvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'file_mapping.c') "/Fo$OutputDirectory/file_mapping.obj"
if ($LASTEXITCODE -ne 0) { throw 'file-mapping fixture compile failed' }
& (Join-Path $taskLlvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$taskSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_file_mapping.exe" "$OutputDirectory/file_mapping.obj" kernel32.lib
if ($LASTEXITCODE -ne 0) { throw 'file-mapping fixture link failed' }
