<#
.SYNOPSIS
Probe authored local and remote live x86 thread context queries.
.DESCRIPTION
Usage: test-live-thread-context.ps1 [-NativeOnly] [-Runner adv-run.exe] [-OutputDirectory target/investigations/...].
Input: LLVM/Windows SDK and a guest runner. Output: 58 scalar DWORD outcomes,
reports, API traces, logs and command/environment/two-hash provenance. No fixed
register values are compared. Query success, requested groups, unrequested
groups, unchanged external suspend counts and continued peer activity are checked.
Only the authored busy worker and one authored child are executed natively.
Running register values can become stale immediately as the peer progresses;
this probe checks successful snapshots without claiming stable live registers.
The native suspend/resume observations test externally visible counts; guest
reads visit the existing quiesced owner without publishing an external suspend.
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/live-thread-context/differential'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'live context evidence must stay beneath workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'live-thread-context.c'
foreach($mode in @('parent','child')){
    $name=if($mode-eq 'parent'){'fixture'}else{'live-context-child'}
    $compileFlags=@()
    if($mode-eq 'child'){$compileFlags=@('/DCHILD')}
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc @compileFlags "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/$name.obj"
    if($LASTEXITCODE-ne 0){throw "live context $name compile failed"}
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$nativeDirectory/$name.exe" "$nativeDirectory/$name.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw "live context $name link failed"}
}
$fixture=Join-Path $nativeDirectory 'fixture.exe';$resultName='live-thread-context.bin';$nativeResult=Join-Path $nativeDirectory $resultName
if(Test-Path -LiteralPath $nativeResult -PathType Leaf){Remove-Item -LiteralPath $nativeResult}
$environment=@("environment PATH: $env:PATH")+@(Get-ChildItem Env:|Where-Object {$_.Name-like 'ADV_*'}|Sort-Object Name|ForEach-Object {"environment $($_.Name): $($_.Value)"})
function Invoke-HiddenProbe {
    param([string]$Executable,[string[]]$Arguments=@(),[string]$Directory,[int]$TimeoutMs,[string]$ChildExecutable='')
    # Start-Process joins ArgumentList, so preserve each Windows argument's quotes.
    $quoted=@($Arguments|ForEach-Object {'"'+([regex]::Replace([regex]::Replace($_,'(\\*)"','$1$1\"'),'(\\+)$','$1$1'))+'"'})
    $options=@{FilePath=$Executable;WorkingDirectory=$Directory;WindowStyle='Hidden';PassThru=$true;RedirectStandardOutput=(Join-Path $Directory 'run.log');RedirectStandardError=(Join-Path $Directory 'stderr.log')}
    if($quoted.Count){$options.ArgumentList=$quoted}
    $process=Start-Process @options
    try{
        if(-not $process.WaitForExit($TimeoutMs)){throw 'live context probe exceeded its bounded runtime'}
        return $process.ExitCode
    }finally{
        try{
            if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null}
            if($ChildExecutable){
                Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)"|Where-Object {$_.ExecutablePath-eq $ChildExecutable}|ForEach-Object {
                    try{$childProcess=[Diagnostics.Process]::GetProcessById($_.ProcessId)}catch [ArgumentException]{return}
                    try{if(-not $childProcess.HasExited){$childProcess.Kill();$childProcess.WaitForExit(1000)|Out-Null}}finally{$childProcess.Dispose()}
                }
            }
        }finally{$process.Dispose()}
    }
}
@('command: fixture.exe (working directory: native)')+$environment+@("source_sha256: $((Get-FileHash -Algorithm SHA256 $source).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $fixture).Hash)")|Set-Content (Join-Path $nativeDirectory 'cmd.txt')
$nativeExit=Invoke-HiddenProbe -Executable $fixture -Directory $nativeDirectory -TimeoutMs 10000 -ChildExecutable (Join-Path $nativeDirectory 'live-context-child.exe')
if($nativeExit-ne 0){throw "native live context failed: $nativeExit"}
$reference=[IO.File]::ReadAllBytes($nativeResult)
if($reference.Length-ne 58*4){throw 'native live context result length failed'}
$flags=@(0x10007,0x10007,0x10001,0x10002,0x10007,0x10007)
for($case=0;$case-lt 6;$case++){
    $row=for($column=0;$column-lt 8;$column++){[BitConverter]::ToUInt32($reference,($case*8+$column)*4)}
    Write-Output "native case=$case result=$($row[0]) LastError=$('{0:x8}'-f $row[1]) flags=$('{0:x8}'-f $row[2]) selected=$($row[3]) unrequested=$($row[4]) suspend=$($row[5]) resume=$($row[6]) active=$($row[7])"
    if($row[0]-ne ($case%2) -or $row[1]-ne 0x13572468 -or $row[2]-ne $flags[$case] -or $row[3]-ne 1 -or $row[4]-ne 1 -or $row[5]-ne 0 -or $row[6]-ne 1 -or $row[7]-ne 1){throw "native live context core contract failed at case $case"}
}
$cleanup=@(0,1,21,1,1,0,1,37,1,1)
for($index=0;$index-lt $cleanup.Count;$index++){if([BitConverter]::ToUInt32($reference,(48+$index)*4)-ne $cleanup[$index]){throw 'native live context cleanup invariant failed'}}
if($NativeOnly){exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory|Out-Null
    foreach($name in @('fixture','live-context-child')){Copy-Item -LiteralPath (Join-Path $nativeDirectory "$name.exe") -Destination (Join-Path $runDirectory "$name.exe") -Force}
    $runFixture=Join-Path $runDirectory 'fixture.exe';$overlay=Join-Path $runDirectory 'data';$report=Join-Path $runDirectory 'report.json';$result=Join-Path $overlay "overlay/$resultName"
    foreach($old in @($report,$result)){if(Test-Path -LiteralPath $old -PathType Leaf){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--trace',(Join-Path $runDirectory 'api.log'),'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("command: adv-run $($arguments-join ' ')")+$environment+@("runner_sha256: $((Get-FileHash -Algorithm SHA256 $Runner).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 $runFixture).Hash)")|Set-Content (Join-Path $runDirectory 'cmd.txt')
    $runnerExit=Invoke-HiddenProbe -Executable $Runner -Arguments $arguments -Directory $runDirectory -TimeoutMs 20000
    $actual=[IO.File]::ReadAllBytes($result)
    if($actual.Length-ne $reference.Length){throw "$variant live context result length failed"}
    for($index=0;$index-lt $reference.Length;$index+=4){if([BitConverter]::ToUInt32($actual,$index)-ne [BitConverter]::ToUInt32($reference,$index)){throw "$variant live context mismatch at DWORD $($index/4)"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json;$root=@($e.api.processes|Where-Object {$null-eq $_.parent});$child=@($e.api.processes|Where-Object {$null-ne $_.parent})
    if($runnerExit-notin @(0,1,37) -or $e.backend-ne $variant -or $e.stop_reason-ne 'guest_exit' -or $e.guest_exit_code-notin @(0,37) -or $e.failure -or $root.Count-ne 1 -or $child.Count-ne 1 -or $root[0].exit_code-ne 0 -or $child[0].exit_code-ne 37 -or $child[0].parent-ne $root[0].pid -or $root[0].failure -or $child[0].failure -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0 -or @($e.api.called_gaps.PSObject.Properties).Count-ne 0){throw "$variant strict live context report failed"}
    if($variant-eq 'jit' -and $e.native_entries-le 0){throw 'live context JIT did not execute translated code'}
    Write-Output "${variant}: all 58 live context scalar outcomes match native"
}
exit 0
