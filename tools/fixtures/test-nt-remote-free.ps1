<#
.SYNOPSIS
Build and compare an authored x86 suspended-child NT free fixture.
.DESCRIPTION
Usage: test-nt-remote-free.ps1 [-Runner adv-run.exe] [-OutputDirectory target/investigations/...] [-NativeOnly].
Input: Windows SDK/LLVM and an adv-run executable. Output: 75 DWORD statistics,
native/backend logs and reports, with commands, environment and two SHA-256 hashes per run.
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/nt-remote-free/differential'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'investigations'+[IO.Path]::DirectorySeparatorChar)){throw 'remote NT free evidence must stay inside target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
$fixture=Join-Path $nativeDirectory 'fixture.exe'
$child=Join-Path $nativeDirectory 'process-child.exe'
foreach($kind in @('fixture','child')){
    $defines=@();if($kind-eq 'child'){$defines=@('/DCHILD')}
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc @defines "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-remote-free.c') "/Fo$nativeDirectory/$kind.obj"
    if($LASTEXITCODE-ne 0){throw "remote NT free $kind compile failed"}
    $executable=$fixture;if($kind-eq 'child'){$executable=$child}
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$executable" "$nativeDirectory/$kind.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw "remote NT free $kind link failed"}
}
@("command: fixture.exe (working directory: native)","environment: inherited; authored fixture, no samples","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $fixture).Hash)","child_sha256: $((Get-FileHash -Algorithm SHA256 $child).Hash)") | Set-Content (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native remote NT free fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native remote NT free fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'nt-remote-free.bin'))
if($reference.Length-ne 75*4){throw 'native remote NT free data length failed'}
$statuses=@{5=0xc0000008L;9=0xc0000022L;13=0xc00000f2L;17=0xc000009fL;21=0xc000001aL;25=0xc0000005L;28=0xc0000005L;34=0;42=0;50=0;55=0;60=0xc00000a0L;68=0xc000010aL}
$lastErrors=@(8,12,16,20,24,27,30,37,45,53,58,63,71)
for($i=0;$i-lt 75;$i++){
    $expected=1
    if($statuses.ContainsKey($i)){$expected=$statuses[$i]}
    elseif($i-in $lastErrors){$expected=0x13572468}
    if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected){throw "native remote NT free invariant $i failed: $([BitConverter]::ToUInt32($reference,$i*4)) vs $expected"}
}
if($NativeOnly){Write-Output 'native: 75 DWORDs of remote NT decommit/recommit/release, INOUT rounding, permissions, nonmutating failures and exited-owner status';exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    $overlay=Join-Path $runDirectory 'data';$report=Join-Path $runDirectory 'report.json'
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    Copy-Item -LiteralPath $fixture -Destination (Join-Path $runDirectory 'fixture.exe')
    Copy-Item -LiteralPath $child -Destination (Join-Path $runDirectory 'process-child.exe')
    $runFixture=Join-Path $runDirectory 'fixture.exe'
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("command: adv-run $($arguments -join ' ')","environment: inherited; authored fixture, no samples","runner_sha256: $((Get-FileHash -Algorithm SHA256 $Runner).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $runFixture).Hash)") | Set-Content (Join-Path $runDirectory 'cmd.txt')
    $runPreference=$ErrorActionPreference
    $ErrorActionPreference='Continue'
    try{& $Runner @arguments *> (Join-Path $runDirectory 'run.log');$runnerExit=$LASTEXITCODE}
    finally{$ErrorActionPreference=$runPreference}
    if($runnerExit-notin @(0,1)){throw "$variant remote NT free fixture could not complete"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/nt-remote-free.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing remote NT free results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 37 -or $e.api.processes.Count-ne 2 -or $e.api.processes[0].exit_code-ne 0 -or $e.api.processes[1].exit_code-ne 37 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict remote NT free execution failed"}
    Write-Output "${variant}: all 75 remote NT free DWORD statistics match native"
}
exit 0
