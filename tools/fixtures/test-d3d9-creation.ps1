param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-creation'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'creation evidence must stay inside target/architecture-acceptance'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
function Words([byte[]]$bytes){if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'};@(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})}
$nativeDirectory=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $nativeDirectory -Name d3d9-creation
foreach($name in @('d3d9-creation.bin','d3d9-creation-pixels.bin')){Remove-FixtureOutput $nativeDirectory $name}
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-creation.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(5000)){throw 'owned native creation fixture exceeded five seconds'};if($native.ExitCode-ne 0){throw "native creation fixture failed: $($native.ExitCode)"}}
finally{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null};$native.Dispose()}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-creation.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-creation-pixels.bin'))
if($reference.Length-ne 770 -or $pixels.Length-ne 6*16*12*4){throw 'native creation evidence length mismatch'}
for($i=0;$i-lt $pixels.Length;$i+=4){if([BitConverter]::ToUInt32($pixels,$i)-ne 0xff2468acu){throw "native independent quad color failed at pixel $($i/4)"}}
if($NativeOnly){
    for($row=0;$row-lt 6;$row++){
        $start=$row*71
        Write-Output ('flags={0:x2}; initial={1}; setters={2}; preReset={3}; postReset={4}' -f $reference[$start+1],$reference[$start+8],(($reference[($start+9)..($start+23)]|ForEach-Object {'{0:x8}'-f $_})-join ','),$reference[$start+54],$reference[$start+56])
    }
    for($i=426;$i-lt 450;$i+=4){Write-Output ('invalid flags={0:x2}; HRESULT={1:x8}; outputNull={2}' -f $reference[$i+1],$reference[$i+2],$reference[$i+3])}
    for($row=0;$row-lt 6;$row++){
        $start=450+$row*53
        for($usage=0;$usage-lt 2;$usage++){
            $at=$start+8+$usage*22
            Write-Output ('buffer flags={0:x2}; fvf={1:x2}; usage={2:x2}; createVB/IB={3:x8}/{4:x8}; SWdrawDP/DIP={5:x8}/{6:x8}; nextHWdraw={7:x8}/{8:x8}' -f $reference[$start+1],$reference[$start+2],($usage*16),$reference[$at],$reference[$at+1],$reference[$at+9],$reference[$at+10],$reference[$at+13],$reference[$at+14])
        }
    }
    Write-Output 'native: 770 DWORDs and 1152 independently checked pixels'
    exit 0
}
$present=@{}
for($row=0;$row-lt 6;$row++){
    $at=$row*71+69
    if($reference[$at-1]-ne 0x90000010u){throw 'creation row Present marker moved'}
    if($reference[$at]-notin @(0u,0x08760878u)){throw 'unexpected native base Present state'}
    $present[$at]=$true
}
if($present.Count-ne 6){throw 'creation comparison must have exactly six Present fields'}
$codes=@{native=@($present.Keys|Sort-Object|ForEach-Object {$reference[$_]})}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-creation.bin','overlay/d3d9-creation-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant creation fixture failed"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-creation.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing creation DWORDs"}
    for($i=0;$i-lt $reference.Length;$i++){
        if($present.ContainsKey($i)){if($actual[$i]-notin @(0u,0x08760878u)){throw "$variant unexpected base Present state"};continue}
        if($actual[$i]-ne $reference[$i]){throw "$variant mismatch at word $i ($($actual[$i]) vs $($reference[$i]))"}
    }
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-creation-pixels.bin'))
    if([Convert]::ToBase64String($actualPixels)-cne [Convert]::ToBase64String($pixels)){throw "$variant exact mode drawing pixels mismatch"}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.frames-ne 6 -or $e.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($e.gpu.live_interfaces-ne 0 -or $e.gpu.pending_waits-ne 0 -or $e.gpu.pending_readbacks-ne 0){throw "$variant COM/map/GPU wait leaked"}
    $commands=$e.gpu.commands
    if($commands.draws-ne 46 -or $commands.vertices-ne 224 -or $commands.submissions-ne 59 -or $commands.readbacks-ne 7 -or $commands.snapshot_allocations-ne 0 -or $commands.vertex_upload_allocations-ne 1){throw "$variant mode draw/upload/readback budget mismatch"}
    if($backend-eq 'jit' -and $e.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    $codes[$variant]=@($present.Keys|Sort-Object|ForEach-Object {$actual[$_]})
    Write-Output "${variant}: 764 exact DWORDs + six Present success comparisons; 1152 exact pixels; entries=$($e.native_entries)"
}
$codes|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $OutputDirectory 'present-success-codes.json')
