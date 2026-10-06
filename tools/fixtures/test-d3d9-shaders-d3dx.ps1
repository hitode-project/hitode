param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shaders-d3dx'),
    [string]$LegacySdkInclude,
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop';$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner);$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'D3DX oracle evidence must stay inside target/architecture-acceptance'}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
function CompareWords($actual,$expected,[string]$label){if($actual.Count-ne $expected.Count){throw "$label output length $($actual.Count) differs from $($expected.Count)"};for($i=0;$i-lt $expected.Count;$i++){if($actual[$i]-ne $expected[$i]){throw "$label DWORD mismatch at $i ($($actual[$i]) vs $($expected[$i]))"}}}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shaders.ps1') -OutputDirectory $nativeDirectory
& (Join-Path $PSScriptRoot 'build-d3d9-shaders-effect.ps1') -OutputDirectory $nativeDirectory -LegacySdkInclude $LegacySdkInclude
$guard=0L;for($i=0;$i-lt 50000;$i++){$guard=($guard*33+$i)-band 0xffffffffL}
# Expected numeric results are independent of the implementation and the native
# reference: int conversion truncates, bool normalizes, matrices pad with zero.
$parameters=@(
 100,0,0x3fc00000u,0,1,0,1,0,0,0xc0400000u,0,0,0x40300000u,0,0,0x3f800000u,
 101,0,0xc0000000u,0,0xfffffffeu,0,1,0,0,0xfffffffdu,0,0,2,0,0,1,
 102,0,0x3f800000u,0,1,0,1,0,0,1,0,0,1,0,0,1,
 200,0,0x3f800000u,0x40000000u,0x40400000u,0,0,0,0x41100000u,0x41000000u,0x40e00000u,
 300,0,0x3f800000u,0x40000000u,0x40400000u,0,0x40800000u,0x40a00000u,0x40c00000u,0,0,0,0,0,0,0,0,0,
 0,0x3f800000u,0x40800000u,0,0,0x40000000u,0x40a00000u,0,0,0x40400000u,0x40c00000u,0,0,0,0,0,0,
 0,0,0x41200000u,0x41300000u,0x41400000u,0x41600000u,0x41700000u,0x41800000u,
 400,0,1,0,0,0,0x3f800000u,0x40000000u,0x40400000u,0x40800000u,0x41100000u,0x41000000u,0x40e00000u,0x40c00000u,0,0x3f800000u,0x40000000u,0x40400000u,0x40800000u,0,0,0,$guard,0x5adef00fu
)
$semantics=@(0,3,0,3,5,0,10,0,0,0,0,1,0,1,10,0,0,1,0,1,10,0,0,3,0,3,0,0,10,0,5,0,0,1,0,1,10,0,0x8876086cu,$guard,0x5ade5e00u)
foreach($oracle in @(@{Name='d3d9-shaders-effect-parameters';Expected=$parameters;Variants=@('interpreter','jit','software-interpreter','software-jit')},@{Name='d3d9-shaders-semantics';Expected=$semantics;Variants=@('interpreter','jit','software-interpreter','software-jit')})){
    $name=$oracle.Name;$fixture=Join-Path $nativeDirectory "fixture_$name.exe";$outputName="$name.bin"
    $native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
    try{if(-not $native.WaitForExit(10000)){throw "owned native $name timeout"};if($native.ExitCode-ne 0){throw "native $name failed: $($native.ExitCode)"}}
    finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
    $reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory $outputName)))
    CompareWords $reference $oracle.Expected "independent native $name"
    if($NativeOnly){Write-Output "native ${name}: $($reference.Count) independently checked DWORDs";continue}
    foreach($variant in $oracle.Variants){
        $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'};$overlay=Join-Path $OutputDirectory "$name-$variant-data";$report=Join-Path $OutputDirectory "$name-$variant.json"
        $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','12','--data-dir',$overlay,'--report',$report)
        if($backend-eq 'jit'){$arguments+='--sync-compilation'}
        if($variant.StartsWith('software')){$arguments+='--gpu-software'}
        & $Runner @arguments *> (Join-Path $OutputDirectory "$name-$variant.log")
        if($LASTEXITCODE-ne 0){throw "$variant $name failed; see log"}
        $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay "overlay/$outputName")))
        CompareWords $actual $reference "$variant $name"
        $e=Get-Content -Raw -LiteralPath $report|ConvertFrom-Json
        if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant $name strict execution failed"}
        if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant $name COM/map/GPU wait leaked"}
        if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant $name did not execute compiled guest code"}
        Write-Output "${variant} ${name}: $($reference.Count) exact native DWORDs, zero leaked interfaces/waits"
    }
}
