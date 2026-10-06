param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shader-models'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner);$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'shader evidence must stay in target/architecture-acceptance'}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$nativeDir=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shader-models.ps1') -OutputDirectory $nativeDir
$fixture=Join-Path $nativeDir 'fixture_d3d9-shader-models.exe'
$process=Start-Process -FilePath $fixture -WorkingDirectory $nativeDir -WindowStyle Hidden -PassThru
try{if(-not $process.WaitForExit(10000)){throw 'native models fixture exceeded ten seconds'};if($process.ExitCode-ne 0){throw "native models fixture failed: $($process.ExitCode)"}}
finally{try{if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null}}finally{$process.Dispose()}}
function Words([byte[]]$bytes){if($bytes.Length%4-ne 0){throw 'partial DWORD'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDir 'd3d9-shader-models.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDir 'd3d9-shader-models-pixels.bin'))
if($reference[-1]-ne 0x5ade0018 -or $pixels.Length-ne 18*16*12*4){throw 'native model oracle incomplete (requires BGRA8 vertex-texture capability)'}
# Independent expected results for the initial texture case, VS1 texture case,
# and PS1.4 range/shift case. Native triangle interpolation on an exact .5 texel
# boundary is exempt; strict interior pixels still constrain the full image.
$texels=@(0xff204080u,0xff80c020u,0xffe04080u,0xff40a0e0u)
foreach($draw in @(0,13,14,15,16,17)){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
    if(($draw-notin @(14,15,16,17)) -and ($x-eq 8 -or $y-eq 6)){continue}
    $want=if($draw-eq 14){0x20ffffffu}elseif($draw-eq 15){0xffffff00u}elseif($draw-eq 16){0xffffffffu}elseif($draw-eq 17){0xff806040u}else{$texels[[int]($x-ge 8)+2*[int]($y-ge 6)]}
    $at=($draw*192+$y*16+$x)*4
    for($channel=0;$channel-lt 4;$channel++){if([Math]::Abs([int]$pixels[$at+$channel]-[int](($want-shr (8*$channel))-band 255))-gt 2){throw "native independent oracle mismatch draw=$draw ($x,$y) channel=$channel"}}
}}}
if($NativeOnly){Write-Output "native: $($reference.Length) DWORDs, 18 draws; VS1/SM1 arithmetic, native DSX/DSY/specials/NORMAL semantics and BGRA8 vertex-texture capability verified";exit 0}
function Compare-Pixels([byte[]]$actual,[byte[]]$reference,[string]$variant){
    if($actual.Length-ne $reference.Length){throw "$variant pixel length mismatch"}
    for($draw=0;$draw-lt 18;$draw++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
        # These are mathematically exact nearest-filter texel boundaries. BEM
        # adds .0625 to U, moving the .5 boundary from column8 to column7.
        if(($draw-in @(0,1,2,3,5,7,8,11,13) -and ($x-eq 8 -or $y-eq 6)) -or ($draw-eq 6 -and $x-eq 7)){continue}
        $at=($draw*192+$y*16+$x)*4
        for($channel=0;$channel-lt 4;$channel++){if([Math]::Abs([int]$actual[$at+$channel]-[int]$reference[$at+$channel])-gt 2){throw "$variant mismatch draw=$draw ($x,$y) ch=$channel host=$($actual[$at+$channel]) native=$($reference[$at+$channel])"}}
    }}}
}
$env:ADV_AUDIO_MUTE_DEVICE='1'
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','90','--d3d9-sm3','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant models fixture failed; see $variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shader-models.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant record length mismatch"}
    for($i=0;$i-lt $actual.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant HRESULT/lifecycle mismatch record=$i host=$($actual[$i]) native=$($reference[$i])"}}
    Compare-Pixels ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shader-models-pixels.bin'))) $pixels $variant
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant leaked shader/GPU resources"}
    if($e.gpu.commands.draws-ne 18 -or $e.gpu.commands.readbacks-ne 18){throw "$variant shader command count mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute JIT"}
    Write-Output "${variant}: $($reference.Length) exact DWORDs, 3201 pixels within two LSB, 18 draws/readbacks, zero leaked resources"
}
