param([string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'))
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$root=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-capabilities'))
$fixtures=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/fixtures'))
New-Item -ItemType Directory -Force -Path $root | Out-Null
function Remove-Output([string]$directory,[string]$name) {
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped fixture directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -Name d3d9-capabilities -OutputDirectory $fixtures
$fixture=Join-Path $fixtures 'fixture_d3d9-capabilities.exe'
Remove-Output $fixtures 'd3d9-capabilities.bin'
$native=Start-Process -FilePath $fixture -WorkingDirectory $fixtures -WindowStyle Hidden -PassThru
if(-not $native.WaitForExit(20000)){
    $native.Kill(); $native.WaitForExit(2000) | Out-Null
    throw 'native capability oracle timed out'
}
if($native.ExitCode -ne 0){throw "native capability oracle failed: $($native.ExitCode)"}
$expected=[IO.File]::ReadAllBytes((Join-Path $fixtures 'd3d9-capabilities.bin'))
if($expected.Length -ne 43*4){throw 'incomplete native capability oracle'}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')) {
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $root "$variant-data"
    $report=Join-Path $root "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-Output $root $name}
    Remove-Output $overlay 'overlay/d3d9-capabilities.bin'
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $root "$variant.log")
    if($LASTEXITCODE -ne 0){throw "$variant failed; see $root/$variant.log"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-capabilities.bin'))
    if([Convert]::ToBase64String($expected) -cne [Convert]::ToBase64String($actual)){throw "$variant native capability contract mismatch"}
    $evidence=Get-Content -Raw $report | ConvertFrom-Json
    if($evidence.guest_exit_code -ne 0 -or $evidence.api.acceptance_eligible -ne $true){throw "$variant exit/strict ABI failure"}
    if($evidence.gpu.live_interfaces -ne 0 -or $evidence.gpu.pending_waits -ne 0 -or $evidence.gpu.pending_readbacks -ne 0){throw "$variant retained GPU ownership"}
    if($backend -eq 'jit' -and $evidence.native_entries -eq 0){throw "$variant did not enter compiled code"}
    Write-Output "${variant}: 43 exact native contract words, zero live COM/GPU waits; native entries=$($evidence.native_entries)"
}
