param(
    [string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-targets'),
    [ValidateSet('d3d9-targets','d3d9-alpha')][string]$FixtureName = 'd3d9-targets'
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'evidence must be inside target/architecture-acceptance'}
function Remove-Output([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
$fixtureDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $fixtureDirectory -Name $FixtureName
$names=@('d3d9-targets.bin','d3d9-targets-pixels.bin')
foreach($name in $names){Remove-Output $fixtureDirectory $name}
$fixture=Join-Path $fixtureDirectory "fixture_$FixtureName.exe"
$native=Start-Process -FilePath $fixture -WorkingDirectory $fixtureDirectory -WindowStyle Hidden -PassThru
try{
    if(-not $native.WaitForExit(5000)){throw 'native targets fixture timed out'}
    if($native.ExitCode -ne 0){throw "native targets fixture failed: $($native.ExitCode)"}
}finally{
    if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}
    $native.Dispose()
}
$reference=@{}
foreach($name in $names){$reference[$name]=[IO.File]::ReadAllBytes((Join-Path $fixtureDirectory $name))}
& {
    # Both fixtures end with Present, device Release, factory Release, guard.
    # This driver's base Present sometimes exposes the positive 9Ex occlusion
    # status. Preserve raw native output; compare success for this one word.
    $presentOffset=$reference['d3d9-targets.bin'].Length-16
    $nativePresent=[BitConverter]::ToUInt32($reference['d3d9-targets.bin'],$presentOffset)
    if($nativePresent -notin @(0u,0x08760878u)){throw 'unexpected native base Present failure'}
    $reference['d3d9-targets.bin']=$reference['d3d9-targets.bin'].Clone()
    [Array]::Clear($reference['d3d9-targets.bin'],$presentOffset,4)
    Write-Output ('Native base Present observation: 0x{0:X8}; one success-status word normalized' -f $nativePresent)
}
$pixels=$reference['d3d9-targets-pixels.bin']
$pixelCount=if($FixtureName -eq 'd3d9-alpha'){256}else{592}
if($pixels.Length -ne $pixelCount*4){throw 'native pixel count mismatch'}
for($i=0;$i -lt $pixelCount;$i++){
    $expected=if($FixtureName -eq 'd3d9-alpha'){0xffaa2200u}elseif($i -lt 64){0xffaa2200u}elseif($i -lt 320){0xff112233u}elseif($i -lt 576){0xffaa2200u}else{0xff00cc44u}
    if([BitConverter]::ToUInt32($pixels,$i*4) -ne $expected){throw "native pixel expectation failed at $i"}
}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-Output $OutputDirectory $name}
    foreach($name in $names){Remove-Output $overlay "overlay/$name"}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','8','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE -ne 0){throw "$variant failed; see log"}
    foreach($name in $names){
        $actual=[IO.File]::ReadAllBytes((Join-Path $overlay "overlay/$name"))
        if([Convert]::ToBase64String($actual) -cne [Convert]::ToBase64String($reference[$name])){throw "$variant $name differs from native"}
    }
    $evidence=Get-Content -Raw $report | ConvertFrom-Json
    if($evidence.guest_exit_code -ne 0 -or $evidence.frames -ne 1 -or $evidence.api.acceptance_eligible -ne $true){throw "$variant strict execution failed"}
    if($evidence.gpu.live_interfaces -ne 0 -or $evidence.gpu.pending_waits -ne 0 -or $evidence.gpu.pending_readbacks -ne 0){throw "$variant resource/wait leak"}
    # Four explicit guest transfers plus the shell's final diagnostic capture.
    $readbacks=if($FixtureName -eq 'd3d9-alpha'){2}else{5}
    if($evidence.gpu.commands.readbacks -ne $readbacks -or $evidence.gpu.commands.snapshot_allocations -ne 0){throw "$variant unexpected transfer or presentation copy"}
    if($backend -eq 'jit' -and $evidence.native_entries -eq 0){throw "$variant had no native execution"}
    $words=$reference['d3d9-targets.bin'].Length/4
    $qualification="$($words-1) exact words plus Present success"
    Write-Output "${variant}: $qualification, $pixelCount exact pixels; no live COM/waits; entries=$($evidence.native_entries)"
}
