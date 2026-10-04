param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-buffers')
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'buffer evidence must stay inside target/architecture-acceptance'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw "output escaped directory: $path"}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
function Words([byte[]]$bytes){
    if(($bytes.Length%4)-ne 0){throw 'partial DWORD output'}
    @(for($i=0;$i-lt $bytes.Length;$i+=4){[BitConverter]::ToUInt32($bytes,$i)})
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $OutputDirectory|Out-Null
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $nativeDirectory -Name d3d9-buffers
foreach($name in @('d3d9-buffers.bin','d3d9-buffers-pixels.bin')){Remove-FixtureOutput $nativeDirectory $name}
$fixture=Join-Path $nativeDirectory 'fixture_d3d9-buffers.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{
    if(-not $native.WaitForExit(5000)){throw 'owned native buffer fixture exceeded five seconds'}
    if($native.ExitCode-ne 0){throw "native buffer fixture failed: $($native.ExitCode)"}
}finally{
    if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}
    $native.Dispose()
}
$reference=Words ([IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-buffers.bin')))
$pixels=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'd3d9-buffers-pixels.bin'))
if($reference.Length-ne 629 -or $pixels.Length-ne 7*16*12*4){throw 'native buffer evidence length mismatch'}
for($i=0;$i-lt $pixels.Length;$i+=4){if([BitConverter]::ToUInt32($pixels,$i)-ne 0xff123456u){throw "native independent quad color failed at pixel $($i/4)"}}
# Preserve and compare all raw fields. Only these specifically authored invalid
# calls differ from this optimized driver's missing checks; do not weaken bounds.
$policy=@{}
$cursor=2
for($pool=0;$pool-lt 4;$pool++){for($usage=0;$usage-lt 3;$usage++){
    $hr=$reference[$cursor++]
    if($hr-eq 0){
        $cursor+=7
        for($flag=0;$flag-lt 8;$flag++){
            $value=$reference[$cursor++]
            $at=$cursor++
            $unlock=$cursor++
            if($value-eq 0x80000000u){$policy[$at]=0x8876086cu;$policy[$unlock]=0xffffffffu}
        }
        $cursor++
    }
}}
$bad=[Array]::IndexOf($reference,0xbad10000u)
if($bad-lt 0){throw 'missing explicit invalid-lock probe marker'}
$policy[$bad+1]=0x8876086cu;$policy[$bad+3]=0x8876086cu
if($policy.Count-ne 18){throw 'invalid-call comparison policy must cover exactly 18 DWORDs'}
$present=$reference.Length-4
if($reference[$present]-notin @(0u,0x08760878u)){throw 'unexpected native base Present state'}
$codes=@{native=$reference[$present]}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    foreach($name in @('overlay/d3d9-buffers.bin','overlay/d3d9-buffers-pixels.bin')){Remove-FixtureOutput $overlay $name}
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant buffer fixture failed; see $OutputDirectory/$variant.log"}
    $actual=Words ([IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-buffers.bin')))
    if($actual.Length-ne $reference.Length){throw "$variant missing buffer DWORDs"}
    $differences=0
    for($i=0;$i-lt $reference.Length;$i++){
        $expected=if($policy.ContainsKey($i)){$policy[$i]}else{$reference[$i]}
        if($i-eq $present){if($actual[$i]-notin @(0u,0x08760878u)){throw "$variant unexpected base Present state"};$codes[$variant]=$actual[$i];continue}
        if($actual[$i]-ne $expected){throw "$variant mismatch at word $i ($($actual[$i]) vs $expected; native raw $($reference[$i]))"}
        if($actual[$i]-ne $reference[$i]){$differences++}
    }
    $actualPixels=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-buffers-pixels.bin'))
    if([Convert]::ToBase64String($actualPixels)-cne [Convert]::ToBase64String($pixels)){throw "$variant exact buffer drawing pixels mismatch"}
    $evidence=Get-Content -Raw $report|ConvertFrom-Json
    if($evidence.guest_exit_code-ne 0 -or $evidence.frames-ne 1 -or $evidence.api.acceptance_eligible-ne $true){throw "$variant strict execution failed"}
    if($evidence.gpu.live_interfaces-ne 0 -or $evidence.gpu.pending_waits-ne 0 -or $evidence.gpu.pending_readbacks-ne 0){throw "$variant COM/map/GPU wait leaked"}
    $commands=$evidence.gpu.commands
    if($commands.draws-ne 7 -or $commands.vertices-ne 38 -or $commands.submissions-ne 22 -or $commands.readbacks-ne 8 -or $commands.snapshot_allocations-ne 0 -or $commands.vertex_upload_allocations-ne 1){throw "$variant draw/upload/readback budget mismatch"}
    if($backend-eq 'jit' -and $evidence.native_entries-eq 0){throw "$variant did not execute compiled guest code"}
    if($differences-ne 18){throw "$variant invalid-call differences changed: $differences"}
    Write-Output "${variant}: 610 exact words + 18 explicit invalid differences + one Present success comparison; 1344 exact pixels; one pooled vertex upload; entries=$($evidence.native_entries)"
}
$codes|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $OutputDirectory 'present-success-codes.json')
