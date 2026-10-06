param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shader-raster')
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'raster evidence must remain under target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shader-raster.ps1') -OutputDirectory $nativeDirectory
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-shader-raster.exe'
$process=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $process.WaitForExit(10000)){throw 'owned native raster oracle timed out'};if($process.ExitCode-ne 0){throw "native raster oracle exit=$($process.ExitCode)"}}
finally{try{if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null}}finally{$process.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shader-raster.bin'))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shader-raster-pixels.bin'))
if($reference.Length-ne 56*4 -or $pixels.Length-ne 3*16*12*4){throw 'native raster output size mismatch'}
for($draw=0;$draw-lt 3;$draw++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
 $expected=switch($draw){0{0xff4000bfu}1{if($x-ge 8){0xffff0000u}else{0xff000000u}}2{if($x-ge 6 -and $x-lt 10 -and $y-ge 4 -and $y-lt 8){0xffff0000u}else{0xff000000u}}}
 $actual=[BitConverter]::ToUInt32($pixels,($draw*192+$y*16+$x)*4)
 if($actual-ne $expected){throw "native raster expectation differs: draw=$draw ($x,$y) actual=$actual expected=$expected"}
}}}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
 $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
 $overlay=Join-Path $OutputDirectory "$variant-data"
 $report=Join-Path $OutputDirectory "$variant.json"
 $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','30','--data-dir',$overlay,'--report',$report)
 if($backend-eq 'jit'){$arguments+='--sync-compilation'}
 if($variant.StartsWith('software')){$arguments+='--gpu-software'}
 & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
 if($LASTEXITCODE-ne 0){throw "$variant raster oracle failed"}
 foreach($pair in @(@('d3d9-shader-raster.bin',$reference),@('d3d9-shader-raster-pixels.bin',$pixels))){
  $actual=[IO.File]::ReadAllBytes((Join-Path $overlay ('overlay/'+$pair[0])))
  if($actual.Length-ne $pair[1].Length){throw "$variant output length differs"}
  if($pair[0]-eq 'd3d9-shader-raster.bin'){
   if([Convert]::ToBase64String($actual)-cne [Convert]::ToBase64String($pair[1])){throw "$variant state differs from native"}
  }else{
   for($i=0;$i-lt $actual.Length;$i++){
    # The clipping plane passes exactly through pixel-centre column8. The
    # software adapter interpolates six zero distances as tiny negatives;
    # all samples strictly on either side retain the exact oracle.
    $pixel=[int][Math]::Floor($i/4)
    if($pixel-ge 192 -and $pixel-lt 384 -and ($pixel%16)-eq 8){continue}
    if($actual[$i]-ne $pair[1][$i]){throw "$variant pixel byte$i differs from native"}
   }
  }
 }
 $e=Get-Content -Raw $report|ConvertFrom-Json
 if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant execution or diagnostics failure"}
 if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
 if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant leaked ownership"}
 Write-Output "${variant}: 56 exact native DWORDs and 564 exact pixels (12 exact-plane boundary samples exempt) (vertex fog, clip plane, point size)"
}
