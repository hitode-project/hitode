<#
.SYNOPSIS
Compare authored x86 NT process termination with Windows and guest backends.
.DESCRIPTION
Usage: test-nt-terminate-process.ps1 [-Runner adv-run.exe] [-OutputDirectory target/investigations/...] [-NativeOnly].
Input: LLVM/Windows SDK and a guest runner. Output: 44 parent and eleven NULL-call
DWORD statistics, strict reports/logs, and commands/environment/two SHA-256 per run.
Reusing an output directory clears only this fixture's prior statistics and report.
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/nt-terminate-process/differential'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'NT termination evidence must stay inside workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$statistics=@('nt-terminate-process','nt-terminate-null','nt-terminate-single-null')
foreach($name in $statistics){$old=Join-Path $nativeDirectory "$name.bin";if(Test-Path -LiteralPath $old -PathType Leaf){Remove-Item -LiteralPath $old}}
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
$names=@('fixture','process-child','self-pseudo','self-owned','null-process','single-null')
for($mode=0;$mode-lt $names.Count;$mode++){
    $name=$names[$mode]
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/DMODE=$mode" "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-terminate-process.c') "/Fo$nativeDirectory/$name.obj"
    if($LASTEXITCODE-ne 0){throw "NT termination $name compile failed"}
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/$name.exe" "$nativeDirectory/$name.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw "NT termination $name link failed"}
}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
@('command: fixture.exe (working directory: native)','environment: inherited; authored fixtures, no samples',"fixture_sha256: $((Get-FileHash -Algorithm SHA256 $fixture).Hash)","null_fixture_sha256: $((Get-FileHash -Algorithm SHA256 (Join-Path $nativeDirectory 'null-process.exe')).Hash)") | Set-Content (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(20000)){throw 'native NT termination exceeded twenty seconds'};if($native.ExitCode-ne 0){throw "native NT termination failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$references=@{}
foreach($name in $statistics){$references[$name]=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory "$name.bin"))}
if($references['nt-terminate-process'].Length-ne 44*4 -or $references['nt-terminate-null'].Length-ne 9*4 -or $references['nt-terminate-single-null'].Length-ne 8){throw 'native NT termination statistic length failed'}
$status=@{2=0xc0000008L;4=0xc0000024L;6=0xc0000024L;8=0xc0000022L;12=0;17=0xc000010aL;25=0xc0000022L}
for($i=0;$i-lt 44;$i++){
    $expected=1
    if($status.ContainsKey($i)){$expected=$status[$i]}
    elseif($i-in @(3,5,7,9,13,18,26)){$expected=0x13572468}
    if([BitConverter]::ToUInt32($references['nt-terminate-process'],$i*4)-ne $expected){throw "native parent invariant $i failed: $([BitConverter]::ToUInt32($references['nt-terminate-process'],$i*4)) vs $expected"}
}
$nullExpected=@(1,0,0x13572468,0,1,0x81234567L,1,0x81234567L,258)
for($i=0;$i-lt $nullExpected.Count;$i++){if([BitConverter]::ToUInt32($references['nt-terminate-null'],$i*4)-ne $nullExpected[$i]){throw "native NULL invariant $i failed: $([BitConverter]::ToUInt32($references['nt-terminate-null'],$i*4)) vs $($nullExpected[$i])"}}
if([BitConverter]::ToUInt32($references['nt-terminate-single-null'],0)-ne 1 -or [BitConverter]::ToUInt32($references['nt-terminate-single-null'],4)-ne 0){throw 'native single-caller NULL returning contract failed'}
if($NativeOnly){Write-Output 'native: 55 DWORD statistics of NT/Zw termination, permissions, exited owner, self handles, NULL peer/last-thread behavior, independent thread exit and first process final status retained';exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    foreach($name in $names){Copy-Item -LiteralPath (Join-Path $nativeDirectory "$name.exe") -Destination (Join-Path $runDirectory "$name.exe")}
    $runFixture=Join-Path $runDirectory 'fixture.exe';$overlay=Join-Path $runDirectory 'data';$report=Join-Path $runDirectory 'report.json'
    $oldFiles=@($report)+@($statistics|ForEach-Object {Join-Path $overlay "overlay/$_.bin"})
    foreach($old in $oldFiles){if(Test-Path -LiteralPath $old -PathType Leaf){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','20','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("command: adv-run $($arguments -join ' ')",'environment: inherited; authored fixtures, no samples',"runner_sha256: $((Get-FileHash -Algorithm SHA256 $Runner).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $runFixture).Hash)") | Set-Content (Join-Path $runDirectory 'cmd.txt')
    $preference=$ErrorActionPreference;$ErrorActionPreference='Continue'
    try{& $Runner @arguments *> (Join-Path $runDirectory 'run.log');$runnerExit=$LASTEXITCODE}
    finally{$ErrorActionPreference=$preference}
    if($runnerExit-notin @(0,1)){throw "$variant NT termination fixture failed"}
    foreach($name in $references.Keys){
        $actual=[IO.File]::ReadAllBytes((Join-Path $overlay "overlay/$name.bin"));$reference=$references[$name]
        if($actual.Length-ne $reference.Length){throw "$variant missing $name statistics"}
        for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant $name mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    }
    $e=Get-Content -Raw $report|ConvertFrom-Json
    $root=@($e.api.processes|Where-Object {$null-eq $_.parent})
    $children=@($e.api.processes|Where-Object {$null-ne $_.parent})
    $expectedChildren=@{'process-child.exe'=0x81234567L;'self-pseudo.exe'=41;'self-owned.exe'=42;'null-process.exe'=0x81234567L;'single-null.exe'=43}
    # The family result can surface a deliberately nonzero child exit. Verify
    # the root and every child owner, including their actual parent identity.
    if($e.backend-ne $variant -or $e.stop_reason-ne 'guest_exit' -or $e.guest_exit_code-notin @(0,0x81234567L) -or $e.failure -or $e.api.processes.Count-ne 6 -or $root.Count-ne 1 -or $children.Count-ne 5 -or $root[0].exit_code-ne 0 -or $root[0].failure -or [IO.Path]::GetFileName($root[0].image)-ne 'fixture.exe' -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0 -or @($e.api.called_gaps.PSObject.Properties).Count-ne 0 -or $e.missing_apis.Count-ne 0){throw "$variant strict NT termination report failed"}
    foreach($name in $expectedChildren.Keys){
        $child=@($children|Where-Object {[IO.Path]::GetFileName($_.image)-eq $name})
        if($child.Count-ne 1 -or $child[0].parent-ne $root[0].pid -or $child[0].exit_code-ne $expectedChildren[$name] -or $child[0].failure){throw "$variant child owner $name exit status differs"}
    }
    Write-Output "${variant}: all 55 NT termination DWORD statistics match native"
}
exit 0
