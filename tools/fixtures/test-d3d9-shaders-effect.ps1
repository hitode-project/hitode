param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shaders-effect'),
    [string]$LegacySdkInclude,
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop';$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner);$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'Effect evidence must stay inside target/architecture-acceptance'}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shaders-effect.ps1') -OutputDirectory $nativeDirectory -LegacySdkInclude $LegacySdkInclude
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-shaders-effect.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'owned native Effect fixture timeout'};if($native.ExitCode-ne 0){throw "native Effect failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shaders-effect.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shaders-effect-pixels.bin'))
if($reference.Length-ne 67 -or $pixels.Length-ne 16*12*4 -or $reference[-1]-ne 0x5adeff01u){throw 'native Effect lifecycle/output length mismatch'}
for($i=0;$i-lt 192;$i++){for($channel=0;$channel-lt 4;$channel++){$expected=@(16,32,64,128)[$channel];if([Math]::Abs([int]$pixels[$i*4+$channel]-$expected)-gt 1){throw "independent native Effect pixel mismatch at $i channel=$channel"}}}
if($NativeOnly){Write-Output 'native: 67 exact DWORDs, 192 independently checked pixels, parameter/technique/pass/preshader/state restoration';exit 0}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'};$overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','12','--data-dir',$overlay,'--report',$report)
    if($backend-eq 'jit'){$arguments+='--sync-compilation'}
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant Effect failed; see $variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shaders-effect.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing Effect DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant Effect DWORD mismatch at $i ($($actual[$i]) vs native $($reference[$i]))"}}
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shaders-effect-pixels.bin'))
    if($actualPixels.Length-ne $pixels.Length){throw "$variant Effect pixel length differs"}
    for($i=0;$i-lt $pixels.Length;$i++){if([Math]::Abs([int]$actualPixels[$i]-[int]$pixels[$i])-gt 1){throw "$variant Effect pixels differ at byte $i"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict Effect execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant Effect COM/map/GPU wait leaked"}
    if($e.gpu.commands.draws-ne 1 -or $e.gpu.commands.readbacks-ne 1){throw "$variant Effect draw/readback budget mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: 67 exact DWORDs, 192 pixels within one channel LSB, one draw/readback, zero leaked interfaces/waits"
}
