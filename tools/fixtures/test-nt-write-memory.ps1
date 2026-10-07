param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/nt-write-memory'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'NT memory write evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-write-memory.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'NT memory write fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'NT memory write fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DCHILD /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-write-memory.c') "/Fo$nativeDirectory/child.obj"
if($LASTEXITCODE-ne 0){throw 'child fixture compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/process-child.exe" "$nativeDirectory/child.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'child fixture link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native NT memory write fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native NT memory write fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'nt-write-memory.bin'))
if($reference.Length-ne 36*4 -or [BitConverter]::ToUInt32($reference,0)-ne 0 -or [BitConverter]::ToUInt32($reference,33*4)-ne 37){throw 'native NT memory write data check failed'}
foreach($i in @(3,4,8,11,14,15,18,29,30,32,34,35)){if([BitConverter]::ToUInt32($reference,$i*4)-ne 1){throw "native write invariant $i failed"}}
foreach($i in @(12,16,20,22)){if([BitConverter]::ToUInt32($reference,$i*4)-ne 2147483661){throw "native partial write status $i failed"}}
if([BitConverter]::ToUInt32($reference,17*4)-ne 4 -or [BitConverter]::ToUInt32($reference,19*4)-ne 0 -or [BitConverter]::ToUInt32($reference,24*4)-ne 0 -or [BitConverter]::ToUInt32($reference,27*4)-ne 0){throw 'native prefix/no-write or best-effort count invariant failed'}
if($NativeOnly){Write-Output 'native: 36 DWORDs of NT memory write permission-preserving writes, restricted handles, count outputs, failures and child exit';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-notin @(0,1)){throw "$variant NT memory write fixture could not complete"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/nt-write-memory.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing NT memory write results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 37 -or $e.api.processes.Count-ne 2 -or $e.api.processes[0].exit_code-ne 0 -or $e.api.processes[1].exit_code-ne 37 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict NT memory write execution failed"}
    Write-Output "${variant}: NT memory write results match native"
}
exit 0
