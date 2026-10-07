param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/ldr-load-dll'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'DLL loading evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'ldr-load-dll.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'DLL loading fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'DLL loading fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DOWNED_DLL /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" (Join-Path $PSScriptRoot 'ldr-load-dll.c') "/Fo$nativeDirectory/owned-dll.obj"
if($LASTEXITCODE-ne 0){throw 'owned DLL compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /dll /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /entry:DllMain@12 /export:fixture_value=_fixture_value@0 "/out:$nativeDirectory/fixture-owned.dll" "$nativeDirectory/owned-dll.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'owned DLL link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DOWNED_DLL /DFAIL_DLL /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" (Join-Path $PSScriptRoot 'ldr-load-dll.c') "/Fo$nativeDirectory/failed-dll.obj"
if($LASTEXITCODE-ne 0){throw 'failing DLL compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /dll /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /entry:DllMain@12 "/out:$nativeDirectory/fixture-failed.dll" "$nativeDirectory/failed-dll.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'failing DLL link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native DLL loading fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native DLL loading fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'ldr-load-dll.bin'))
if($reference.Length-ne 20*4 -or [BitConverter]::ToUInt32($reference,0)-ne 3221225781 -or [BitConverter]::ToUInt32($reference,7*4)-ne 17){throw 'native DLL loading data check failed'}
$expected=@([uint32]3221225781,[uint32]0x12345678,[uint32]0x13572468,0,1,[uint32]0x24681357,1,17,0,1,1,1,1,1,34,0,[uint32]3221225794,[uint32]0x12345678,[uint32]0x24681357,1)
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native DLL loading invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output 'native: 20 DWORDs of counted UTF-16 DLL loading, reference lifetime, initializer rollback and LastError';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant DLL loading fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/ldr-load-dll.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing DLL loading results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict DLL loading execution failed"}
    Write-Output "${variant}: DLL loading results match native"
}
