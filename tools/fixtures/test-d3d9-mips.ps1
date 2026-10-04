param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-mips'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'mip evidence must stay inside target/architecture-acceptance'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $nativeDirectory -Name d3d9-mips
foreach($name in @('d3d9-mips.bin','d3d9-mips-pixels.bin')){Remove-FixtureOutput $nativeDirectory $name}
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-mips.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(5000)){throw 'owned native mip fixture exceeded five seconds'};if($native.ExitCode-ne 0){throw "native mip fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-mips.bin')))
$pixels=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-mips-pixels.bin')))
if($reference.Length-ne 422 -or $reference[-1]-ne 0xcafef00du -or $pixels.Length-ne 249){throw "incomplete native mip evidence: $($reference.Length) words/$($pixels.Length) pixels"}
# Uniform mip contents make all point/disabled samples and selected-level
# readbacks exact. Only the two true half-way linear pixels permit one LSB.
$colors=@('ffff0000','ffffffff','ffff0000','ffff0000','ff0000ff','ff0000ff','ffffffff','ff8080ff','ff0000ff','ffffffff','ff00ff00','ffff0000','ff00ff00','ff0000ff','ffffffff','ffffffff')
$lengths=@(1,1,32,1,32,32,32,2,32,32,8,32,8,2,1,1)
$at=0
for($group=0;$group-lt $colors.Length;$group++){
    $expected=[Convert]::ToUInt32($colors[$group],16)
    for($i=0;$i-lt $lengths[$group];$i++){
        if($group-eq 7){
            $actual=[BitConverter]::GetBytes([uint32]$pixels[$at+$i])
            $want=[BitConverter]::GetBytes($expected)
            for($channel=0;$channel-lt 4;$channel++){if([Math]::Abs([int]$actual[$channel]-[int]$want[$channel])-gt 1){throw "native linear mip color failed at pixel $($at+$i)"}}
        }elseif($pixels[$at+$i]-ne $expected){throw "native mip color group $group failed at pixel $($at+$i)"}
    }
    $at+=$lengths[$group]
}
Write-Output "native: $($reference.Length) DWORDs, $($pixels.Length) defined mip pixels"
if($NativeOnly){return}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-mips.bin','overlay/d3d9-mips-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant mip fixture failed; see $OutputDirectory/$variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-mips.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant incomplete mip DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant mip mismatch at word $i ($($actual[$i]) vs native $($reference[$i]))"}}
    $actualPixels=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-mips-pixels.bin')))
    if($actualPixels.Length-ne $pixels.Length){throw "$variant incomplete mip pixels"}
    for($i=0;$i-lt $pixels.Length;$i++){
        if($i-ge 131 -and $i-lt 133){
            $want=[BitConverter]::GetBytes([uint32]$pixels[$i]);$got=[BitConverter]::GetBytes([uint32]$actualPixels[$i])
            for($channel=0;$channel-lt 4;$channel++){if([Math]::Abs([int]$want[$channel]-[int]$got[$channel])-gt 1){throw "$variant linear mip mismatch at pixel $i/$channel"}}
        }elseif($actualPixels[$i]-ne $pixels[$i]){throw "$variant exact mip mismatch at pixel $i"}
    }
    $evidence=Get-Content -Raw $report|ConvertFrom-Json
    if($evidence.guest_exit_code-ne 0 -or $evidence.frames-ne 0 -or $evidence.api.acceptance_eligible-ne $true){throw "$variant strict mip execution failed"}
    if($evidence.gpu.live_interfaces-ne 0 -or $evidence.gpu.pending_waits-ne 0 -or $evidence.gpu.pending_readbacks-ne 0){throw "$variant mip COM/map/GPU wait leaked"}
    $commands=$evidence.gpu.commands
    if($commands.draws-ne 11 -or $commands.vertices-ne 44 -or $commands.readbacks-ne 16 -or $commands.submissions-ne 42 -or $commands.vertex_upload_allocations-ne 1 -or $commands.snapshot_allocations-ne 0){throw "$variant mip draw/readback budget mismatch"}
    if($backend-eq 'jit' -and $evidence.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: $($reference.Length) exact mip words, $($pixels.Length) mip pixels (linear 1 LSB), 11 draws/16 readbacks, no leaked interfaces/maps; entries=$($evidence.native_entries)"
}
