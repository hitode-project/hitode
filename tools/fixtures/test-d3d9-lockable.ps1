param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-lockable'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'fixture evidence must stay inside workspace target'}
$native=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $native -Name d3d9-lockable
function Read-Records([string]$Path) {
    $bytes=[IO.File]::ReadAllBytes($Path)
    if($bytes.Length%8-ne 0){throw 'incomplete tagged fixture evidence'}
    $records=@{}
    for($at=0;$at-lt $bytes.Length;$at+=8){
        $tag=[BitConverter]::ToUInt32($bytes,$at)
        if($records.ContainsKey($tag)){throw 'duplicate fixture tag'}
        $records[$tag]=[BitConverter]::ToUInt32($bytes,$at+4)
    }
    if($records[[uint32]999]-ne [Convert]::ToUInt32('D3D9BACC',16)){throw 'missing completion marker'}
    return $records
}
$process=Start-Process -FilePath (Join-Path $native 'fixture_d3d9-lockable.exe') -WorkingDirectory $native -WindowStyle Hidden -PassThru
try { if(-not $process.WaitForExit(5000)){throw 'owned native fixture timed out'};if($process.ExitCode-ne 0){throw 'owned native fixture failed'} }
finally { if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null};$process.Dispose() }
$reference=Read-Records (Join-Path $native 'd3d9-lockable.bin')
foreach($offset in 1..48){if($reference[[uint32](500+$offset)]-ne $reference[[uint32](600+$offset)]){throw 'native no-specular FVF pixels changed with SPECULARENABLE'}}
if($reference[[uint32]501]-ne [Convert]::ToUInt32('80262820',16) -or $reference[[uint32]548]-ne [Convert]::ToUInt32('FF112233',16)){throw 'native specular comparison did not draw the gradient and clear background'}
Write-Output "Native: $($reference.Count) tagged results, including 288 exact color pixels."
if($NativeOnly){return}
foreach($backend in @('interpreter','jit')){
    $folder=Join-Path $OutputDirectory $backend
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    $arguments=@((Join-Path $native 'fixture_d3d9-lockable.exe'),'--headless','--backend',$backend,'--seconds','5','--data-dir',(Join-Path $folder 'data'),'--report',(Join-Path $folder 'report.json'))
    $argumentLine=($arguments | ForEach-Object {'"'+$_+'"'}) -join ' '
    $process=Start-Process -FilePath $Runner -ArgumentList $argumentLine -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $folder 'stdout.log') -RedirectStandardError (Join-Path $folder 'stderr.log')
    try { if(-not $process.WaitForExit(10000)){throw "guest $backend timed out"};if($process.ExitCode-ne 0){throw "guest $backend failed"} }
    finally { if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null};$process.Dispose() }
    $actual=Read-Records (Join-Path $folder 'data/overlay/d3d9-lockable.bin')
    $checked=0
    foreach($tag in $reference.Keys){
        # Nonblocking LockRect is not implemented in this slice. Preserve the
        # native result and explicitly require INVALIDCALL in the guest.
        if($tag-eq 60 -or $tag-eq 61){continue}
        if(-not $actual.ContainsKey($tag) -or $actual[$tag]-ne $reference[$tag]){throw ('guest {0}, tag {1}: expected {2:X8}, got {3:X8}' -f $backend,$tag,$reference[$tag],$actual[$tag])}
        $checked++
    }
    foreach($tag in $actual.Keys){if(-not $reference.ContainsKey($tag) -and $tag-ne 61){throw "unexpected guest tag $tag"}}
    if($actual[[uint32]60]-ne [Convert]::ToUInt32('8876086C',16) -or $actual.ContainsKey([uint32]61)){throw 'unexpected nonblocking lock contract'}
    $report=Get-Content -LiteralPath (Join-Path $folder 'report.json') -Raw | ConvertFrom-Json
    if($report.guest_exit_code-ne 0 -or $report.api.acceptance_eligible-ne $true){throw 'strict fixture acceptance failed'}
    Write-Output "$backend`: $checked exact tagged comparisons; DONOTWAIT remains explicitly unsupported."
}
