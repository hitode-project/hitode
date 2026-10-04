param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-clear'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'clear evidence must stay inside target/architecture-acceptance'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $nativeDirectory -Name d3d9-clear
foreach($name in @('d3d9-clear.bin','d3d9-clear-pixels.bin')){Remove-FixtureOutput $nativeDirectory $name}
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-clear.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(5000)){throw 'owned native clear fixture exceeded five seconds'};if($native.ExitCode-ne 0){throw "native clear fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-clear.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-clear-pixels.bin'))
if($reference.Length-ne 206 -or $pixels.Length-ne 13*16*12*4){throw "native clear length mismatch: $($reference.Length) DWORDs/$($pixels.Length) pixel bytes"}
if($reference[-1]-ne 0xc1ea000du){throw 'missing final clear lifecycle marker'}
foreach($start in @(167,194)){
    $full=@(0,0,16,12)
    for($i=0;$i-lt 4;$i++){if($reference[$start+$i]-ne $full[$i]){throw 'native SetRenderTarget did not reset scissor to the full target'}}
}
for($group=0;$group-lt 13;$group++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
    $expected=switch($group){
        0{if($x-lt 8){0xff00ff00u}else{0xffff0000u}}
        1{if($x-lt 8){0xff000000u}else{0xff0000ffu}}
        2{if($x-lt 8){0xff00ffffu}else{0xff000000u}}
        3{if($x-lt 8){0xff00ffffu}else{0xffffff00u}}
        4{if($y-lt 6){0xff00ff00u}else{0xff000000u}}
        5{if($x-lt 8){0xff000000u}else{0xffff0000u}}
        6{if($y-lt 6 -or $x-ge 8){0xff0000ffu}else{0xff000000u}}
        7{if($y-lt 6){0xffff00ffu}else{0xff000000u}}
        {$_-in 8,9}{if($y-ge 6){0xff000000u}elseif($x-lt 8){0xff112233u}else{0xffff00ffu}}
        10{if($x-lt 8){0xff000000u}else{0xff0000ffu}}
        11{if($x-lt 8 -and $y-lt 6){0xff00ff00u}else{0xff000000u}}
        12{if($x-lt 8){0xff445566u}else{0xff112233u}}
    }
    $at=($group*192+$y*16+$x)*4
    if([BitConverter]::ToUInt32($pixels,$at)-ne $expected){throw "native independent clear pixel failed: group=$group ($x,$y), actual=$([BitConverter]::ToUInt32($pixels,$at)) expected=$expected"}
}}}
if($NativeOnly){Write-Output 'native: 206 DWORDs, 2496 independently checked viewport color/depth/stencil pixels';exit 0}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-clear.bin','overlay/d3d9-clear-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant clear fixture failed"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-clear.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing clear DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant clear mismatch at word $i ($($actual[$i]) vs native $($reference[$i]))"}}
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-clear-pixels.bin'))
    if([Convert]::ToBase64String($actualPixels)-cne [Convert]::ToBase64String($pixels)){throw "$variant viewport color/depth/stencil pixels differ"}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.frames-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant COM/map/GPU wait leaked"}
    $commands=$e.gpu.commands
    if($commands.draws-ne 15 -or $commands.vertices-ne 70 -or $commands.submissions-ne 38 -or $commands.readbacks-ne 13 -or $commands.snapshot_allocations-ne 0 -or $commands.vertex_upload_allocations-ne 1){throw "$variant viewport draw/upload/readback budget mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: 206 exact DWORDs, 2496 exact pixels, 15 draws/70 vertices/38 submissions/13 readbacks, zero snapshots; entries=$($e.native_entries)"
}
