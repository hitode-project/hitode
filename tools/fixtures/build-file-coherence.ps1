param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/file-coherence-tests'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$coherenceSdk='C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x86'
& 'C:/Program Files/LLVM/bin/clang-cl.exe' /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'file_coherence.c') "/Fo$OutputDirectory/file_coherence.obj"
if($LASTEXITCODE -ne 0){throw 'Own coherence fixture compile failed'}
& 'C:/Program Files/LLVM/bin/lld-link.exe' /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$coherenceSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_file_coherence.exe" "$OutputDirectory/file_coherence.obj" kernel32.lib
if($LASTEXITCODE -ne 0){throw 'Own coherence fixture link failed'}
