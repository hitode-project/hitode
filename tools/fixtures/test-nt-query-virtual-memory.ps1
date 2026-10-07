<#
.SYNOPSIS
Compare authored x86 basic NT VM queries with Windows and guest backends.
.DESCRIPTION
Usage: test-nt-query-virtual-memory.ps1 [-Runner adv-run.exe] [-OutputDirectory target/investigations/...] [-NativeOnly].
Input: LLVM/Windows SDK and a guest runner. Output: 168 DWORD outcomes for
32 local/remote query cases, strict reports/logs and command/environment hashes.
121 DWORDs compare the shared contract with hosted Windows; the remaining 47
check native x86 buffer probing/access rules separately from WOW64 translation.
Only this authored parent and suspended child are executed natively.
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/nt-query-virtual-memory/differential'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'NT query evidence must stay inside workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'nt-query-virtual-memory.c'
foreach($mode in @(0,1)){
    $name=if($mode-eq 0){'fixture'}else{'query-child'}
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/DCHILD=$mode" "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/$name.obj"
    if($LASTEXITCODE-ne 0){throw "NT query $name compile failed"}
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/$name.exe" "$nativeDirectory/$name.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw "NT query $name link failed"}
}
$fixture=Join-Path $nativeDirectory 'fixture.exe';$resultName='nt-query-virtual-memory.bin'
$nativeResult=Join-Path $nativeDirectory $resultName
if(Test-Path -LiteralPath $nativeResult -PathType Leaf){Remove-Item -LiteralPath $nativeResult}
$environment=@("environment PATH: $env:PATH")+@(Get-ChildItem Env: | Where-Object {$_.Name -like 'ADV_*'} | Sort-Object Name | ForEach-Object {"environment $($_.Name): $($_.Value)"})
@('command: fixture.exe (working directory: native)')+$environment+@("source_sha256: $((Get-FileHash -Algorithm SHA256 $source).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $fixture).Hash)") | Set-Content (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native NT query exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native NT query failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes($nativeResult)
if($reference.Length-ne 168*4){throw 'native NT query statistic length failed'}
for($index=0;$index-lt 32;$index++){
    if([BitConverter]::ToUInt32($reference,($index*5+1)*4)-ne 0x13572468){throw "native LastError changed at case $index"}
}
for($index=160;$index-lt 168;$index++){if([BitConverter]::ToUInt32($reference,$index*4)-ne 1){throw "native cleanup invariant $index failed"}}
if($NativeOnly){
    for($index=0;$index-lt 32;$index++){
        $row=for($column=0;$column-lt 5;$column++){[BitConverter]::ToUInt32($reference,($index*5+$column)*4)}
        Write-Output "case=$index status=$('{0:x8}'-f $row[0]) length=$('{0:x8}'-f $row[2]) unchanged=$($row[3]) query_match=$($row[4])"
    }
    exit 0
}
# A hosted 32-bit fixture runs through WOW64. Its NULL/alignment translation,
# limited-information rights and failure ReturnLength publication differ from
# the native x86 contract. Preserve all raw evidence and compare only shared
# behavior with that oracle; validate the profile-specific rules independently.
$commonCases=@(0,1,2,3,4,5,6,7,8,9,10,11,14,17,18,19,20,22,25,27,28,29,30,31)
$failureLengthCases=@(9,10,11,27,29,30,31)
$nativeX86Cases=@{
    12=@(3221225477,0x13572468,0x24681357,1,0)
    13=@(3221225600,0x13572468,0x24681357,1,0)
    15=@(3221225600,0x13572468,2779096485,1,0)
    16=@(3221225600,0x13572468,0,1,0)
    21=@(3221225600,0x13572468,0x24681357,1,0)
    23=@(3221225477,0x13572468,2779096485,1,0)
    24=@(3221225477,0x13572468,0x24681357,1,0)
    26=@(3221225506,0x13572468,0x24681357,1,0)
}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    foreach($name in @('fixture','query-child')){Copy-Item -LiteralPath (Join-Path $nativeDirectory "$name.exe") -Destination (Join-Path $runDirectory "$name.exe") -Force}
    $runFixture=Join-Path $runDirectory 'fixture.exe';$overlay=Join-Path $runDirectory 'data';$report=Join-Path $runDirectory 'report.json';$result=Join-Path $overlay "overlay/$resultName"
    foreach($old in @($report,$result)){if(Test-Path -LiteralPath $old -PathType Leaf){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("command: adv-run $($arguments-join ' ')")+$environment+@("runner_sha256: $((Get-FileHash -Algorithm SHA256 $Runner).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $runFixture).Hash)") | Set-Content (Join-Path $runDirectory 'cmd.txt')
    $preference=$ErrorActionPreference;$ErrorActionPreference='Continue'
    try{& $Runner @arguments *> (Join-Path $runDirectory 'run.log');$runnerExit=$LASTEXITCODE}
    finally{$ErrorActionPreference=$preference}
    $actual=[IO.File]::ReadAllBytes($result)
    if($actual.Length-ne $reference.Length){throw "$variant NT query statistic length failed"}
    $common=0;$profile=0
    foreach($case in $commonCases){
        for($column=0;$column-lt 5;$column++){
            $index=($case*5+$column)*4
            if($column-eq 2 -and $case-in $failureLengthCases){
                if([BitConverter]::ToUInt32($actual,$index)-ne 0x24681357){throw "$variant native x86 failure ReturnLength changed at case $case"}
                $profile++;continue
            }
            if([BitConverter]::ToUInt32($actual,$index)-ne [BitConverter]::ToUInt32($reference,$index)){throw "$variant NT query native mismatch at case $case column $column"}
            $common++
        }
    }
    foreach($case in $nativeX86Cases.Keys){
        for($column=0;$column-lt 5;$column++){
            if([BitConverter]::ToUInt32($actual,($case*5+$column)*4)-ne $nativeX86Cases[$case][$column]){throw "$variant native x86 query contract failed at case $case column $column"}
            $profile++
        }
    }
    for($index=160;$index-lt 168;$index++){
        if([BitConverter]::ToUInt32($actual,$index*4)-ne [BitConverter]::ToUInt32($reference,$index*4)){throw "$variant NT query cleanup invariant $index failed"}
        $common++
    }
    if($common-ne 121 -or $profile-ne 47){throw 'NT query contract coverage count failed'}
    $e=Get-Content -LiteralPath $report -Raw|ConvertFrom-Json
    $root=@($e.api.processes|Where-Object {$null-eq $_.parent});$child=@($e.api.processes|Where-Object {$null-ne $_.parent})
    if($runnerExit-notin @(0,1,37) -or $e.backend-ne $variant -or $e.stop_reason-ne 'guest_exit' -or $e.guest_exit_code-notin @(0,37) -or $e.failure -or $root.Count-ne 1 -or $child.Count-ne 1 -or $root[0].exit_code-ne 0 -or $child[0].exit_code-ne 37 -or $child[0].parent-ne $root[0].pid -or $root[0].failure -or $child[0].failure -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0 -or @($e.api.called_gaps.PSObject.Properties).Count-ne 0){throw "$variant strict NT query report failed"}
    if($variant-eq 'jit' -and $e.native_entries-le 0){throw 'NT query JIT fixture did not execute translated guest code'}
    Write-Output "${variant}: 121 common NT query DWORDs match hosted native; 47 native x86 contract DWORDs verified separately"
}
exit 0
