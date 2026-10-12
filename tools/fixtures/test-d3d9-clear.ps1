<#
.SYNOPSIS
Compare authored viewport and unequal-attachment Clear calls with native D3D9.
.DESCRIPTION
Builds its own fixture, independently checks defined pixels, then compares
interpreter/JIT on hardware/software adapters. Input is a frozen runner and an
isolated target/investigations output directory. Output contains counts only;
native binaries, hashes, command records and pixel evidence stay local.
.EXAMPLE
./tools/fixtures/test-d3d9-clear.ps1 -Runner <runner> -OutputDirectory <evidence>
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/d3d9-clear'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'clear evidence must stay inside target/investigations'}
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
$fixtureHash=(Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash.ToLowerInvariant()
@("command=$fixture",'environment=native Windows; owned hidden fixture',"binary_sha256=$fixtureHash","guest_exe_sha256=$fixtureHash") | Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(5000)){throw 'owned native clear fixture exceeded five seconds'};if($native.ExitCode-ne 0){throw "native clear fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-clear.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-clear-pixels.bin'))
if($reference.Length-ne 455 -or $pixels.Length-ne 27*16*12*4){throw "native clear length mismatch: $($reference.Length) DWORDs/$($pixels.Length) pixel bytes"}
if($reference[-1]-ne 0xc1ea000du){throw 'missing final clear lifecycle marker'}
foreach($start in @(167,194)){
    $full=@(0,0,16,12)
    for($i=0;$i-lt 4;$i++){if($reference[$start+$i]-ne $full[$i]){throw 'native SetRenderTarget did not reset scissor to the full target'}}
}
for($group=0;$group-lt 27;$group++){for($y=0;$y-lt 12;$y++){for($x=0;$x-lt 16;$x++){
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
        13{if($x-lt 8 -and $y-lt 6){0xff000000u}else{0xff00ff00u}}
        14{if($x-lt 8 -and $y-lt 6){0xff0000ffu}else{0xff000000u}}
        {$_-in 15,16}{0xff00ff00u}
        26{0xffff0000u}
        {$_-ge 17 -and $_-le 25}{
            $set=if($_-eq 25){7}else{$_-17};$dimensions=$set%4+1
            0xff000000u -bor $(if($set-band 1){0xff0000u}else{0u}) -bor $(if($dimensions-ge 2 -and ($set-band 2)){0xff00u}else{0u}) -bor $(if($dimensions-ge 3 -and ($set-band 4)){0xffu}else{0u})
        }
    }
    $at=($group*192+$y*16+$x)*4
    if([BitConverter]::ToUInt32($pixels,$at)-ne $expected){throw "native independent clear pixel failed: group=$group ($x,$y), actual=$([BitConverter]::ToUInt32($pixels,$at)) expected=$expected"}
}}}
if($NativeOnly){Write-Output 'native: 455 DWORDs, 5184 independently checked viewport, attachment, POSITIONT and texture-coordinate pixels';exit 0}
$runnerHash=(Get-FileHash -LiteralPath $Runner -Algorithm SHA256).Hash.ToLowerInvariant()
@("binary_sha256=$runnerHash","guest_exe_sha256=$fixtureHash",'environment=ADV_AUDIO_MUTE_DEVICE=1') | Set-Content -LiteralPath (Join-Path $OutputDirectory 'cmd.txt')
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-clear.bin','overlay/d3d9-clear-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','15','--d3d9-sm2','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    "command=$Runner $($arguments -join ' ')" | Add-Content -LiteralPath (Join-Path $OutputDirectory 'cmd.txt')
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
    if($commands.draws-ne 30 -or $commands.vertices-ne 134 -or $commands.submissions-ne 82 -or $commands.readbacks-ne 27 -or $commands.snapshot_allocations-ne 0 -or $commands.vertex_upload_allocations-ne 3){throw "$variant viewport draw/upload/readback budget mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: 455 exact DWORDs, 5184 exact pixels, 30 draws/134 vertices/82 submissions/27 readbacks, three upload allocations, zero snapshots; entries=$($e.native_entries)"
}
