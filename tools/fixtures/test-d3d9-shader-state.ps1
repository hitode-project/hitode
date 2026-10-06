param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-shader-state'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'shader state evidence must stay inside target/architecture-acceptance'}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9-shader-state.ps1') -OutputDirectory $nativeDirectory
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-shader-state.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native shader state fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native shader state fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-shader-state.bin')))
if($reference[-1]-ne 0x5a7e0001 -or $reference[4]-ne 2 -or $reference[5]-ne 0xffffffffu -or $reference[6]-ne 0x80000000u){throw 'native shader state lifecycle/BOOL check failed'}
if($NativeOnly){Write-Output "native: $($reference.Length) exact DWORDs, preserved BOOL words, bounded GetFunction copy, zero-public shader lifetime, typed/custom state blocks, Reset";exit 0}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','12','--data-dir',$overlay,'--report',$report)
    if($backend-eq 'jit'){$arguments+='--sync-compilation'}
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant shader state fixture failed; see $variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-shader-state.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing shader state DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){if($actual[$i]-ne $reference[$i]){throw "$variant shader state mismatch at $i ($($actual[$i]) vs native $($reference[$i]))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant shader state COM/wait leak"}
    if($e.gpu.shader_versions.vertex.'2.0'-ne 2 -or $e.gpu.shader_versions.malformed-ne 0 -or $e.gpu.shader_versions.untranslated-ne 0){throw "$variant shader model diagnostic mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    Write-Output "${variant}: $($reference.Length) exact DWORDs match native, zero leaked interfaces/waits"
}
