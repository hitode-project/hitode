param(
    [string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9')
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if($OutputDirectory -ne $acceptanceRoot -and -not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
    throw 'D3D9 evidence directory must be inside target/architecture-acceptance'
}
$fixtureDirectory=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/fixtures'))
function Remove-FixtureOutput([string]$directory,[string]$name){
    # Only named files may be removed. Resolve and check the final absolute path
    # before removal, including overlay paths; never recursively remove a tree.
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
        throw "fixture output escaped its directory: $path"
    }
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $fixtureDirectory
$fixture=Join-Path $fixtureDirectory 'fixture_d3d9.exe'
foreach($name in @('d3d9-result.bin','d3d9-pixels.bin')){Remove-FixtureOutput $fixtureDirectory $name}
$native=Start-Process -FilePath $fixture -WorkingDirectory $fixtureDirectory -WindowStyle Hidden -PassThru
if(-not $native.WaitForExit(20000)){
    $native.Kill()
    $native.WaitForExit(2000) | Out-Null
    throw 'native D3D9 fixture timed out after 20 seconds'
}
if($native.ExitCode -ne 0){throw "native D3D9 fixture failed: $($native.ExitCode)"}
$referenceResults=[IO.File]::ReadAllBytes((Join-Path $fixtureDirectory 'd3d9-result.bin'))
$referencePixels=[IO.File]::ReadAllBytes((Join-Path $fixtureDirectory 'd3d9-pixels.bin'))
# Independent pixel expectation: nearest 2x2 texels, with the documented D3D9
# half-pixel placement covering the entire 16x12 render target.
if($referencePixels.Length -ne 16*12*4){throw 'native pixel byte count mismatch'}
$texels=@(0xff0000ffu,0xff00ff00u,0xffff0000u,0xffffffffu)
for($y=0;$y -lt 12;$y++){for($x=0;$x -lt 16;$x++){
    $expected=$texels[([int]($y -ge 6))*2+[int]($x -ge 8)]
    if([BitConverter]::ToUInt32($referencePixels,($y*16+$x)*4) -ne $expected){throw "native unexpected pixel at $x,$y"}
}}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.png","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-result.bin','overlay/d3d9-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report,'--screenshot',(Join-Path $OutputDirectory "$variant.png"))
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE -ne 0){throw "$variant fixture failed; see $OutputDirectory/$variant.log"}
    $results=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-result.bin'))
    $pixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-pixels.bin'))
    if([Convert]::ToBase64String($results) -cne [Convert]::ToBase64String($referenceResults)){throw "$variant HRESULT/refcount/native state mismatch"}
    if([Convert]::ToBase64String($pixels) -cne [Convert]::ToBase64String($referencePixels)){throw "$variant exact BGRA mismatch"}
    $evidence=Get-Content -Raw $report | ConvertFrom-Json
    if($evidence.guest_exit_code -ne 0 -or $evidence.frames -ne 1 -or $evidence.api.acceptance_eligible -ne $true){throw "$variant execution/strict ABI acceptance failed"}
    if($evidence.gpu.live_interfaces -ne 0 -or $evidence.gpu.pending_waits -ne 0 -or $evidence.gpu.pending_readbacks -ne 0){throw "$variant GPU/COM ownership leaked"}
    if($evidence.gpu.commands.readbacks -ne 2 -or $evidence.gpu.commands.submissions -ne 4 -or $evidence.gpu.commands.snapshot_allocations -ne 0){throw "$variant command/readback/presentation accounting mismatch"}
    if($backend -eq 'jit' -and $evidence.native_entries -eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: $($results.Length/4) exact native words, 192 exact pixels, no live COM/map wait; compiled entries=$($evidence.native_entries)"
}
