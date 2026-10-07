param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/empty-relocation-directory'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'empty relocation evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'empty-relocation-directory.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'empty relocation fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'empty relocation fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DOWNED_DLL /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" (Join-Path $PSScriptRoot 'empty-relocation-directory.c') "/Fo$nativeDirectory/owned-dll.obj"
if($LASTEXITCODE-ne 0){throw 'owned DLL compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /dll /machine:x86 /nodefaultlib /dynamicbase /base:0x50000000 /safeseh:no /timestamp:0 /entry:DllMain@12 /include:_relocation_block /export:fixture_value=_fixture_value@0 "/out:$nativeDirectory/fixture-template.dll" "$nativeDirectory/owned-dll.obj"
if($LASTEXITCODE-ne 0){throw 'owned DLL link failed'}
$source=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'fixture-template.dll'))
$nt=[BitConverter]::ToInt32($source,0x3c);$sections=[BitConverter]::ToUInt16($source,$nt+6);$optional=[BitConverter]::ToUInt16($source,$nt+20);$rva=$null
for($i=0;$i-lt $sections;$i++){$at=$nt+24+$optional+$i*40;if([Text.Encoding]::ASCII.GetString($source,$at,8).TrimEnd([char]0)-eq '.fixture'){$rva=[BitConverter]::ToUInt32($source,$at+12);break}}
if($null-eq $rva){throw 'authored relocation section missing'}
foreach($profile in @(@('fixture-empty.dll',$true,$false),@('fixture-none.dll',$false,$false),@('fixture-stripped.dll',$false,$true))){
 $bytes=$source.Clone();$relocationRva=if($profile[1]){[uint32]$rva}else{[uint32]0};$relocationSize=if($profile[1]){[uint32]8}else{[uint32]0}
 [BitConverter]::GetBytes($relocationRva).CopyTo($bytes,$nt+24+96+5*8);[BitConverter]::GetBytes($relocationSize).CopyTo($bytes,$nt+24+96+5*8+4)
 $flags=[BitConverter]::ToUInt16($bytes,$nt+22);$flags=if($profile[2]){$flags-bor 1}else{$flags-band 65534};[BitConverter]::GetBytes([uint16]$flags).CopyTo($bytes,$nt+22)
 [IO.File]::WriteAllBytes((Join-Path $nativeDirectory $profile[0]),$bytes)
}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native empty relocation fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native empty relocation fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'empty-relocation-directory.bin'))
if($reference.Length-ne 19*4 -or [BitConverter]::ToUInt32($reference,0)-ne 1 -or [BitConverter]::ToUInt32($reference,16*4)-ne 1){throw 'native relocation output incomplete'}
$expected=@(1,0,1,[uint32]0x13572468,17,1,0,1,[uint32]0x13572468,17,1,[uint32]3221225496,1,[uint32]0x13572468,0,1,1,1,487)
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native relocation invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output 'native: 19 DWORDs of zero-fixup rebase, callable exports, stripped rejection and NT/Win32 errors';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant empty relocation fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/empty-relocation-directory.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing empty relocation results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict empty relocation execution failed"}
    Write-Output "${variant}: empty relocation results match native"
}
