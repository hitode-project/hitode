param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-border'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'border evidence must stay inside target/architecture-acceptance'}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-border.ps1') -OutputDirectory $nativeDirectory
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-border.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native border fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native border fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-border.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-border-pixels.bin'))
if($reference[-1]-ne 0xb0ade014u -or $pixels.Length-ne 56 -or [BitConverter]::ToUInt32($pixels,0)-ne 0x80102040u -or $pixels[35]-ne 255 -or $pixels[39]-ne 255){throw 'native border completion or arbitrary-color check failed'}
if($NativeOnly){Write-Output "native: $($reference.Length) exact DWORDs and14 border nearest/linear/mip/mixed-address/RGBX pixels";exit 0}
foreach($variant in @('hardware-jit','hardware-interpreter','software-jit','software-interpreter')){
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$(if($variant.EndsWith('interpreter')){'interpreter'}else{'jit'}),'--seconds','12','--data-dir',$overlay,'--report',$report)
    if($variant.EndsWith('jit')){$arguments+='--sync-compilation'}
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant border fixture failed; see $variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-border.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing border DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant border API mismatch at $i ($($actual[$i]) vs native $($reference[$i]))"}}
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-border-pixels.bin'))
    if($actualPixels.Length-ne $pixels.Length){throw "$variant missing border pixels"}
    for($i=0;$i-lt $pixels.Length;$i++){if([Math]::Abs([int]$actualPixels[$i]-[int]$pixels[$i])-gt 1){throw "$variant border pixel mismatch at byte$i ($($actualPixels[$i]) vs native $($pixels[$i]))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant border COM/wait leak"}
    if($e.gpu.commands.draws-ne 14 -or $e.gpu.commands.readbacks-ne 14){throw "$variant border draw/readback budget mismatch"}
    if($variant.EndsWith('jit') -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: $($reference.Length) exact DWORDs and14 native border pixels, zero leaked interfaces/waits"
}
