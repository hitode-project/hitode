param([string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),[string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-version'),[switch]$NativeOnly)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'factory evidence must stay inside workspace target'}
$native=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $native -Name 'd3d9-version'
$resultPath=Join-Path $native 'd3d9-version.bin'
if(Test-Path -LiteralPath $resultPath){Remove-Item -LiteralPath $resultPath -Force}
$process=Start-Process -FilePath (Join-Path $native 'fixture_d3d9-version.exe') -WorkingDirectory $native -WindowStyle Hidden -PassThru
try{if(-not $process.WaitForExit(5000)){throw 'owned factory probe timed out'};if($process.ExitCode-ne 0){throw 'owned factory probe failed'}}
finally{if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null};$process.Dispose()}
$reference=[IO.File]::ReadAllBytes($resultPath)
if($reference.Length-ne 164 -or [BitConverter]::ToUInt32($reference,160)-ne [Convert]::ToUInt32('D3D93132',16)){throw 'incomplete factory evidence'}
for($index=0;$index-lt 8;++$index){'SDK {0:X8}: factory={1}, refs={2}/{3}/{4}' -f [BitConverter]::ToUInt32($reference,$index*20),[BitConverter]::ToUInt32($reference,$index*20+4),[BitConverter]::ToUInt32($reference,$index*20+8),[BitConverter]::ToUInt32($reference,$index*20+12),[BitConverter]::ToUInt32($reference,$index*20+16)}
if($NativeOnly){return}
foreach($backend in @('interpreter','jit')){
    $folder=Join-Path $OutputDirectory $backend
    New-Item -ItemType Directory -Force $folder | Out-Null
    $actualPath=Join-Path $folder 'data/overlay/d3d9-version.bin'
    if(Test-Path -LiteralPath $actualPath){Remove-Item -LiteralPath $actualPath -Force}
    & $Runner (Join-Path $native 'fixture_d3d9-version.exe') --headless --backend $backend --seconds 5 --data-dir (Join-Path $folder 'data') --report (Join-Path $folder 'report.json') *> (Join-Path $folder 'run.log')
    if($LASTEXITCODE-ne 0){throw "factory guest $backend failed"}
    $actual=[IO.File]::ReadAllBytes($actualPath)
    if($actual.Length-ne $reference.Length){throw 'incomplete guest factory evidence'}
    for($index=0;$index-lt 41;++$index){
        $expected=[BitConverter]::ToUInt32($reference,$index*4)
        # D3D_DEBUG_INFO object layouts are outside this normal-SDK slice.
        # Keep the native raw successes, and require guest NULL/no references.
        if($index-ge 25 -and $index-lt 40 -and $index%5-ne 0){$expected=[uint32]0}
        if([BitConverter]::ToUInt32($actual,$index*4)-ne $expected){throw "factory guest $backend word $index differs"}
    }
    $report=Get-Content -LiteralPath (Join-Path $folder 'report.json') -Raw | ConvertFrom-Json
    if($report.guest_exit_code-ne 0 -or $report.api.acceptance_eligible-ne $true){throw 'factory strict acceptance failed'}
    Write-Output "$backend`: 26 exact native words + 3 explicitly unsupported high-bit cases (15 words)"
}
