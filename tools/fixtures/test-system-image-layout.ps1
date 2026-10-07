param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/system-image-layout'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'system image evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'system-image-layout.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'system image fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib user32.lib
if($LASTEXITCODE-ne 0){throw 'system image fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DCHILD /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'system-image-layout.c') "/Fo$nativeDirectory/child.obj"
if($LASTEXITCODE-ne 0){throw 'child fixture compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/process-child.exe" "$nativeDirectory/child.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'child fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DOWNED_DLL /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" (Join-Path $PSScriptRoot 'ldr-load-dll.c') "/Fo$nativeDirectory/owned-dll.obj"
if($LASTEXITCODE-ne 0){throw 'owned DLL compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /dll /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /entry:DllMain@12 /export:fixture_value=_fixture_value@0 "/out:$nativeDirectory/fixture-owned.dll" "$nativeDirectory/owned-dll.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'owned DLL link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native system image fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native system image fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'system-image-layout.bin'))
$expected=@(1,1,1,1,0,37,1,1,1,1,0,1,17,[uint32]0x24681357,1,1,1,1)
if($reference.Length-ne 18*4){throw 'native system image results missing'}
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native system image invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output 'native: 18 DWORDs of initialization list and system image identity, real child calls parent loader pointer';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-notin @(0,1)){throw "$variant system image fixture could not complete"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/system-image-layout.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing system image results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 37 -or $e.api.processes.Count-ne 2 -or $e.api.processes[0].exit_code-ne 0 -or $e.api.processes[1].exit_code-ne 37 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict system image execution failed"}
    Write-Output "${variant}: system image results match native"
}
exit 0
