param(
    [string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-present')
)
$ErrorActionPreference = 'Stop'
$env:ADV_AUDIO_MUTE_DEVICE = '1'
$Runner = [IO.Path]::GetFullPath($Runner)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot + [IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
    throw 'D3D9 evidence directory must be inside target/architecture-acceptance'
}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path = [IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory + [IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
        throw "output escaped directory: $path"
    }
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$fixtureDirectory = Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $fixtureDirectory -Name d3d9-present
Remove-FixtureOutput $fixtureDirectory 'd3d9-present.bin'
$fixture = Join-Path $fixtureDirectory 'fixture_d3d9-present.exe'
$native = Start-Process -FilePath $fixture -WorkingDirectory $fixtureDirectory -WindowStyle Hidden -PassThru
try {
    if(-not $native.WaitForExit(5000)){throw 'native D3D9 Present fixture timed out after five seconds'}
    if($native.ExitCode -ne 0){throw "native D3D9 Present fixture failed: $($native.ExitCode)"}
} finally {
    # Only the owned process is terminated, and cleanup has its own deadline.
    if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000) | Out-Null}
    $native.Dispose()
}
$reference = [IO.File]::ReadAllBytes((Join-Path $fixtureDirectory 'd3d9-present.bin'))
if($reference.Length -ne 91*4){throw 'native Present output size mismatch'}
for($case = 0; $case -lt 3; $case++){
    $base = $case*30
    $interval = @(0u,1u,0x80000000u)[$case]
    if([BitConverter]::ToUInt32($reference,$base*4) -ne $interval -or [BitConverter]::ToUInt32($reference,($base+1)*4) -ne 0){throw "native creation failed in case $case"}
    for($state = 0; $state -lt 5; $state++){
        $at = $base+2+$state*5+[int]($state -ge 3)*2
        $expected = @([uint32]$state,[uint32]($state -in @(1,2,3)),[uint32]($state -eq 2),0u,0u)
        for($word = 0; $word -lt 5; $word++){
            if([BitConverter]::ToUInt32($reference,($at+$word)*4) -ne $expected[$word]){throw "native unexpected window/Present/cooperative state at case $case, state $state, word $word"}
        }
    }
    if([BitConverter]::ToUInt32($reference,($base+17)*4) -ne 1 -or [BitConverter]::ToUInt32($reference,($base+18)*4) -ne 1){throw 'native SHOWNA unexpectedly restored minimized window'}
    if([BitConverter]::ToUInt32($reference,($base+29)*4) -ne 0){throw 'native device did not release'}
}
if([BitConverter]::ToUInt32($reference,90*4) -ne 0){throw 'native factory did not release'}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend = if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay = Join-Path $OutputDirectory "$variant-data"
    $report = Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    Remove-FixtureOutput $overlay 'overlay/d3d9-present.bin'
    $arguments = @($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments += '--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE -ne 0){throw "$variant failed; see $OutputDirectory/$variant.log"}
    $actual = [IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-present.bin'))
    if([Convert]::ToBase64String($actual) -cne [Convert]::ToBase64String($reference)){throw "$variant native Present visibility matrix mismatch"}
    $evidence = Get-Content -Raw $report | ConvertFrom-Json
    if($evidence.guest_exit_code -ne 0 -or $evidence.frames -ne 15 -or $evidence.api.acceptance_eligible -ne $true){throw "$variant strict execution failed"}
    if($evidence.gpu.live_interfaces -ne 0 -or $evidence.gpu.pending_waits -ne 0 -or $evidence.gpu.pending_readbacks -ne 0){throw "$variant COM/wait ownership leaked"}
    if($backend -eq 'jit' -and $evidence.native_entries -eq 0){throw "$variant no compiled guest entries"}
    Write-Output "${variant}: 91 exact native words; 15 successful hidden/shown/minimized/restored/hidden base Presents; SHOWNA retains minimization; entries=$($evidence.native_entries)"
}
