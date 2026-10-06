param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shaders'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
# A shared target directory can be passed from an isolated checkout.
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'shader evidence must stay inside target/architecture-acceptance'}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
function Pixels-Close([byte[]]$actual,[byte[]]$reference,[string]$variant){
    if($actual.Length-ne 5*16*12*4 -or $reference.Length-ne $actual.Length){throw "$variant pixel length mismatch"}
    for($group=0;$group-lt 5;$group++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
        # Native D3D9 point filtering and cmp at an exact interpolated 0.5
        # boundary vary by rounding. Interior pixels retain the full oracle.
        if(($group-eq 1 -and ($x-eq 8 -or $y-eq 6)) -or ($group-eq 4 -and $x-eq 8)){continue}
        $at=($group*192+$y*16+$x)*4
        for($channel=0;$channel-lt 4;$channel++){if([Math]::Abs([int]$actual[$at+$channel]-[int]$reference[$at+$channel])-gt 1){throw "$variant pixel mismatch: draw=$group ($x,$y) channel=$channel actual=$($actual[$at+$channel]) native=$($reference[$at+$channel])"}}
    }}}
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shaders.ps1') -OutputDirectory $nativeDirectory
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-shaders.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'owned native shader fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native shader fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shaders.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shaders-pixels.bin'))
if($reference.Length-ne 123 -or $pixels.Length-ne 5*16*12*4 -or $reference[-1]-ne 0x5ade0005u){throw 'native shader lifecycle/output length mismatch'}
for($group=0;$group-lt 5;$group++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
    if(($group-eq 1 -and ($x-eq 8 -or $y-eq 6)) -or ($group-eq 4 -and $x-eq 8)){continue}
    $expected=switch($group){
        {$_-in 0,3}{0xff401840u}
        1{if($y-lt 6){if($x-lt 8){0xff101080u}else{0xff403020u}}else{if($x-lt 8){0xff701080u}else{0xff2028e0u}}}
        2{0xff806040u}
        4{if($x-lt 8){0xff0000ffu}else{0xffff0000u}}
    }
    $actual=[BitConverter]::ToUInt32($pixels,($group*192+$y*16+$x)*4)
    if($actual-ne $expected){throw "native independent shader pixel mismatch: draw=$group ($x,$y) actual=$actual expected=$expected"}
}}}
if($NativeOnly){Write-Output 'native: 123 exact DWORDs, five 16x12 draws; 921 independently checked pixels, boundary exemptions documented';exit 0}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','12','--data-dir',$overlay,'--report',$report)
    if($backend-eq 'jit'){$arguments+='--sync-compilation'}
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant shader fixture failed; see $variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shaders.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing shader DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant shader DWORD mismatch at $i ($($actual[$i]) vs native $($reference[$i]))"}}
    Pixels-Close ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shaders-pixels.bin'))) $pixels $variant
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant COM/map/GPU wait leaked"}
    if($e.gpu.commands.draws-ne 5 -or $e.gpu.commands.readbacks-ne 5){throw "$variant shader draw/readback budget mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: 123 exact DWORDs, 921 pixels within one channel LSB, five draws/readbacks, zero leaked interfaces/waits"
}
