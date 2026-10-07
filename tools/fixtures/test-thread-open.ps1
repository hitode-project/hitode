<#
.SYNOPSIS
Compare an authored thread-open fixture on native Windows, interpreter and JIT.
.DESCRIPTION
Runner selects adv-run. OutputDirectory keeps fixture binaries, scalar results,
reports, logs and cmd.txt provenance under this workspace's target/investigations.
NativeOnly skips guest replay. Checks OA/CID capture, output failure atomicity,
access restrictions, inheritance, LastError and owned local/child lifetimes.
The child is authored from the same source. No sample or host thread is opened.
.EXAMPLE
pwsh tools/fixtures/test-thread-open.ps1 -NativeOnly
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/thread-open'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'thread-open evidence must stay inside workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10'
$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'thread-open.c'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'thread-open fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'thread-open fixture link failed'}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /DCHILD /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/child.obj"
if($LASTEXITCODE-ne 0){throw 'thread-open child compile failed'}
$childFixture=Join-Path $nativeDirectory 'thread-open-child.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$childFixture" "$nativeDirectory/child.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'thread-open child link failed'}
@("Command: $fixture","Working directory: $nativeDirectory","Environment PATH: $env:PATH","Source SHA-256: $((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash)","Binary SHA-256: $((Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash)","Child SHA-256: $((Get-FileHash -LiteralPath $childFixture -Algorithm SHA256).Hash)") | Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native thread-open fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native thread-open fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'thread-open.bin'))
$sentinel=[uint32]0x24681357
$maximum=[uint32]::MaxValue
$invalidCid=[Convert]::ToUInt32('c000000b',16)
$parameterMix=[Convert]::ToUInt32('c0000030',16)
$accessViolation=[Convert]::ToUInt32('c0000005',16)
$invalidParameter=[Convert]::ToUInt32('c000000d',16)
$accessDenied=[Convert]::ToUInt32('c0000022',16)
$statuses=@(0,0,$invalidCid,$invalidCid,$invalidCid,$parameterMix,$accessViolation,$invalidParameter,$invalidParameter,$parameterMix,0,$accessViolation,0,0,0,$invalidParameter,$accessDenied,0,0)
$expected=@()
for($index=0;$index-lt $statuses.Count;$index++){
    $status=$statuses[$index];$unchanged=$index-in @(7,8,11)
    $expected+=@($status,[uint32](-not $unchanged),[uint32]($status-ne 0 -and -not $unchanged),$sentinel,[uint32]($index-eq 12))
}
$expected+=@($accessViolation,$sentinel,1,$sentinel,$maximum,5,1,$sentinel,0,$sentinel,1,1,259,$maximum,5,$maximum,5,0,1,1,21)
$expected+=@(0,1,$sentinel,1,259,$maximum,5,$maximum,5,0,1,$sentinel,1,1,1,0,1,37,0,0,1,$sentinel,1,37)
$expected+=@(1,87,1,5,1,$sentinel,$accessViolation,1,$sentinel,0,1,$sentinel,0,0,$sentinel,0,0,$sentinel)
if($reference.Length-ne $expected.Count*4){throw 'native thread-open results missing'}
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native thread-open invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output "native: $($expected.Count) thread-open owner, access, OA/CID, publication and LastError outcomes verified";exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    $runFixture=Join-Path $runDirectory 'fixture.exe'
    Copy-Item -LiteralPath $fixture -Destination $runFixture -Force
    Copy-Item -LiteralPath $childFixture -Destination (Join-Path $runDirectory 'thread-open-child.exe') -Force
    $overlay=Join-Path $runDirectory 'data'
    $report=Join-Path $runDirectory 'report.json'
    $resultFile=Join-Path $overlay 'overlay/thread-open.bin'
    foreach($old in @($report,$resultFile)){if(Test-Path -LiteralPath $old){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("Command runner: $Runner","Arguments: $($arguments | ConvertTo-Json -Compress)","Working directory: $((Get-Location).Path)","Environment PATH: $env:PATH","Fixture SHA-256: $((Get-FileHash -LiteralPath $runFixture -Algorithm SHA256).Hash)","Runner SHA-256: $((Get-FileHash -LiteralPath $Runner -Algorithm SHA256).Hash)","Child SHA-256: $((Get-FileHash -LiteralPath $childFixture -Algorithm SHA256).Hash)") | Set-Content -LiteralPath (Join-Path $runDirectory 'cmd.txt')
    & $Runner @arguments *> (Join-Path $runDirectory 'run.log')
    $runnerExit=$LASTEXITCODE
    $actual=[IO.File]::ReadAllBytes($resultFile)
    if($actual.Length-ne $reference.Length){throw "$variant missing thread-open results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native thread-open mismatch at DWORD $($i/4)"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    $root=@($e.api.processes|Where-Object {$null-eq $_.parent})
    $child=@($e.api.processes|Where-Object {$null-ne $_.parent})
    # A family run may surface the child's deliberately nonzero exit status.
    # Verify both actual owners rather than treating that status as parent failure.
    if($runnerExit-notin @(0,1,37) -or $e.guest_exit_code-notin @(0,37) -or $root.Count-ne 1 -or $child.Count-ne 1 -or $root[0].exit_code-ne 0 -or $child[0].exit_code-ne 37 -or $root[0].failure -or $child[0].failure -or $e.failure -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0){throw "$variant strict thread-open execution failed (runner=$runnerExit)"}
    Write-Output "${variant}: all $($expected.Count) thread-open outcomes match native"
}
exit 0
