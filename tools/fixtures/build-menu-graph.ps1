<#
Build the authored x86 menu graph/resource fixture from menu_graph.c and menu_graph.rc.
Checks cover recursive edits, popup metadata, ownership, child IDs, templates, and class menu resources.
Inputs are synthetic menu resources; windows remain invisible and no interactive popup is opened.
Run from the repository root: ./tools/fixtures/build-menu-graph.ps1
Build requires LLVM and the Windows SDK; default output is target/menu-graph-tests.
Run fixture_menu_graph.exe from that directory to write menu-graph.bin.
Native/guest comparison: cargo test --release -p adv-runtime --test menu_graph -- --test-threads=1
#>
param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/menu-graph-tests'))
$ErrorActionPreference='Stop'
$OutputDirectory=[System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'menu_graph.c') "/Fo$OutputDirectory/menu_graph.obj"
if ($LASTEXITCODE -ne 0) { throw 'menu graph fixture compile failed' }
& (Join-Path $llvm 'llvm-rc.exe') /nologo "/fo$OutputDirectory/menu_graph.res" (Join-Path $PSScriptRoot 'menu_graph.rc')
if ($LASTEXITCODE -ne 0) { throw 'menu graph resource compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_menu_graph.exe" "$OutputDirectory/menu_graph.obj" "$OutputDirectory/menu_graph.res" kernel32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'menu graph fixture link failed' }
