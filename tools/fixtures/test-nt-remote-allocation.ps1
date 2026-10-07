param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/nt-remote-allocation'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'remote NT allocation evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-remote-allocation.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'remote NT allocation fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'remote NT allocation fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DCHILD /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-remote-allocation.c') "/Fo$nativeDirectory/child.obj"
if($LASTEXITCODE-ne 0){throw 'child fixture compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/process-child.exe" "$nativeDirectory/child.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'child fixture link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native remote NT allocation fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native remote NT allocation fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'nt-remote-allocation.bin'))
if($reference.Length-ne 38*4 -or [BitConverter]::ToUInt32($reference,0)-ne 1 -or [BitConverter]::ToUInt32($reference,30*4)-ne 37){throw 'native remote NT allocation data check failed'}
foreach($i in @(2,3,7,9,13,15,17,19,20,22,23,24,27,29)){if([BitConverter]::ToUInt32($reference,$i*4)-ne 1){throw "native remote allocation invariant $i failed"}}
if([BitConverter]::ToUInt32($reference,5*4)-ne 0 -or [BitConverter]::ToUInt32($reference,6*4)-ne 8192 -or [BitConverter]::ToUInt32($reference,18*4)-ne 3221225496 -or [BitConverter]::ToUInt32($reference,21*4)-ne 3221225541){throw 'native remote allocation status/size failed'}
if([BitConverter]::ToUInt32($reference,32*4)-ne 3221225485 -or [BitConverter]::ToUInt32($reference,34*4)-ne 3221225485 -or [BitConverter]::ToUInt32($reference,35*4)-ne 3221225477 -or [BitConverter]::ToUInt32($reference,36*4)-ne 3221225477){throw 'native NT allocation argument validation failed'}
if($NativeOnly){Write-Output 'native: 38 DWORDs of remote NT allocation, Win32 query/transfer/free interop, isolation and child exit';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-notin @(0,1)){throw "$variant remote NT allocation fixture could not complete"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/nt-remote-allocation.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing remote NT allocation results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 37 -or $e.api.processes.Count-ne 2 -or $e.api.processes[0].exit_code-ne 0 -or $e.api.processes[1].exit_code-ne 37 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict remote NT allocation execution failed"}
    Write-Output "${variant}: remote NT allocation results match native"
}
exit 0
