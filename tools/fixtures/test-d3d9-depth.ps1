param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-depth')
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'depth evidence must stay inside target/architecture-acceptance'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw "output escaped directory: $path"}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
function Words([byte[]]$bytes){
    if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'}
    @(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $OutputDirectory|Out-Null
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $nativeDirectory -Name d3d9-depth
foreach($name in @('d3d9-depth.bin','d3d9-depth-pixels.bin')){Remove-FixtureOutput $nativeDirectory $name}
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-depth.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{
    if(-not $native.WaitForExit(5000)){throw 'owned native depth fixture exceeded five seconds'}
    if($native.ExitCode-ne 0){throw "native depth fixture failed: $($native.ExitCode)"}
}finally{
    try{
        if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}
    }finally{$native.Dispose()}
}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-depth.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-depth-pixels.bin'))
if($reference.Length-ne 256 -or $pixels.Length-ne 11*16*12*4){throw "native depth evidence length mismatch: $($reference.Length) words, $($pixels.Length) pixel bytes"}
if($reference[255]-ne [Convert]::ToUInt32('cafef00d',16)){throw 'missing final depth lifecycle marker'}
$expectedColors=@('ffff0000','ff0000ff','ff000000','ffffffff','ff00ff00','ff00ff00','ff0000ff','ff00ffff','ffff00ff','ffffff00')
for($group=0;$group-lt 10;$group++){
    $color=[Convert]::ToUInt32($expectedColors[$group],16)
    for($i=0;$i-lt 192;$i++){
        if([BitConverter]::ToUInt32($pixels,($group*192+$i)*4)-ne $color){throw "native defined depth color group $group failed at pixel $i"}
    }
}
$faces=Words ($pixels[7680..($pixels.Length-1)])
$colors=@($faces|Sort-Object -Unique)
if($colors.Length-ne 3 -or $colors-notcontains [Convert]::ToUInt32('ff000000',16) -or $colors-notcontains [Convert]::ToUInt32('ffff0000',16) -or $colors-notcontains [Convert]::ToUInt32('ff00ff00',16)){throw 'native two-sided stencil did not produce two different triangle colors and untouched background'}
foreach($color in @('ffff0000','ff00ff00','ff000000')){
    $expected=if($color-eq 'ff000000'){96}else{48}
    if(@($faces|Where-Object {$_-eq [Convert]::ToUInt32($color,16)}).Length-ne $expected){throw "native two-sided triangle coverage count failed: $color"}
}
$invalid=[Convert]::ToUInt32('8876086c',16)
foreach($index in @(212,230,246)){if($reference[$index]-ne $invalid){throw "native required depth-only/Reset failure at word $index was $($reference[$index])"}}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-depth.bin','overlay/d3d9-depth-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant depth fixture failed; see $OutputDirectory/$variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-depth.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing depth DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){
        if($actual[$i]-ne $reference[$i]){throw "$variant depth mismatch at word $i ($($actual[$i]) vs native $($reference[$i]))"}
    }
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-depth-pixels.bin'))
    if([Convert]::ToBase64String($actualPixels)-cne [Convert]::ToBase64String($pixels)){throw "$variant exact depth/stencil drawing pixels mismatch"}
    $evidence=Get-Content -Raw $report|ConvertFrom-Json
    if($evidence.guest_exit_code-ne 0 -or $evidence.frames-ne 0 -or $evidence.api.acceptance_eligible-ne $true){throw "$variant strict depth execution failed"}
    if($evidence.gpu.live_interfaces-ne 0 -or $evidence.gpu.pending_waits-ne 0 -or $evidence.gpu.pending_readbacks-ne 0){throw "$variant depth COM/map/GPU wait leaked"}
    $commands=$evidence.gpu.commands
    if($commands.draws-ne 20 -or $commands.vertices-ne 82 -or $commands.readbacks-ne 11 -or $commands.submissions-ne 40 -or $commands.vertex_upload_allocations-ne 1 -or $commands.snapshot_allocations-ne 0){throw "$variant depth draw/readback budget mismatch"}
    if($backend-eq 'jit' -and $evidence.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: 256 exact depth words, 2112 exact pixels, 20 draws/82 vertices/11 readbacks, zero leaked interfaces/maps; submissions=$($commands.submissions); entries=$($evidence.native_entries)"
}
