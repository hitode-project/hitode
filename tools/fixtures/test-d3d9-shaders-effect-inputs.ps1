param(
    [Parameter(Mandatory=$true)][string]$Manifest,
    [Parameter(Mandatory=$true)][string]$Runner,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [string]$LegacySdkInclude,
    [ValidateSet('jit','interpreter')][string]$Backend='jit'
)
# The manifest is local evidence: {inputs:[{family:'label',path:'private.fxo'}]}.
# Copy input bytes into target only. Neither names nor bytes enter the summary.
$ErrorActionPreference='Stop';$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory);$Manifest=[IO.Path]::GetFullPath($Manifest);$Runner=[IO.Path]::GetFullPath($Runner)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar)){throw 'private Effect evidence must stay inside target'}
$inputs=@((Get-Content -Raw -LiteralPath $Manifest|ConvertFrom-Json).inputs)
if($inputs.Count-eq 0 -or $inputs.Count-gt 4096){throw 'local Effect input count must be 1..4096'}
$buildDirectory=Join-Path $OutputDirectory 'harness'
& (Join-Path $PSScriptRoot 'build-d3d9-shaders-effect.ps1') -OutputDirectory $buildDirectory -LegacySdkInclude $LegacySdkInclude
$summary=@();$ordinal=0
foreach($input in $inputs){
    $directory=Join-Path $OutputDirectory ('input-'+$ordinal.ToString('D4'));New-Item -ItemType Directory -Force -Path $directory|Out-Null
    $privateSource=[IO.Path]::GetFullPath([string]$input.path)
    if((Get-Item -LiteralPath $privateSource).Length-gt 16*1024*1024){throw "private Effect input $ordinal exceeds byte limit"}
    Copy-Item -LiteralPath $privateSource -Destination (Join-Path $directory 'effect-input.fxo') -Force
    $fixture=Join-Path $directory 'fixture_d3d9-shaders-effect-input.exe'
    Copy-Item -LiteralPath (Join-Path $buildDirectory 'fixture_d3d9-shaders-effect-input.exe') -Destination $fixture -Force
    $native=Start-Process -FilePath $fixture -WorkingDirectory $directory -WindowStyle Hidden -PassThru
    try{if(-not $native.WaitForExit(10000)){throw "owned native Effect input $ordinal timeout"};$nativeExit=$native.ExitCode}
    finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
    $overlay=Join-Path $directory 'guest-data';$report=Join-Path $directory 'guest.json'
    $arguments=@($fixture,'--hidden','--backend',$Backend,'--d3d9-sm3','--seconds','12','--data-dir',$overlay,'--report',$report)
    if($Backend-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $directory 'guest.log');$runnerExit=$LASTEXITCODE
    $nativePath=Join-Path $directory 'effect-input-result.bin';$guestPath=Join-Path $overlay 'overlay/effect-input-result.bin'
    $exact=(Test-Path -LiteralPath $nativePath)-and (Test-Path -LiteralPath $guestPath)-and [Convert]::ToBase64String([IO.File]::ReadAllBytes($nativePath))-ceq [Convert]::ToBase64String([IO.File]::ReadAllBytes($guestPath))
    $diagnostics=if(Test-Path -LiteralPath $report){Get-Content -Raw -LiteralPath $report|ConvertFrom-Json}else{$null}
    $strict=($diagnostics -and $diagnostics.api.acceptance_eligible-eq $true -and $diagnostics.gpu.pending_waits-eq 0 -and $diagnostics.gpu.pending_readbacks-eq 0 -and ($Backend-ne 'jit' -or $diagnostics.native_entries-gt 0))
    $summary+=[PSCustomObject]@{family=[string]$input.family;ordinal=$ordinal;native_exit=$nativeExit;runner_exit=$runnerExit;guest_exit=if($diagnostics){$diagnostics.guest_exit_code}else{$null};exact_records=$exact;strict=$strict;live_interfaces=if($diagnostics){$diagnostics.gpu.live_interfaces}else{$null}}
    $ordinal++
}
$aggregate=@($summary|Group-Object family|ForEach-Object {[PSCustomObject]@{family=$_.Name;inputs=$_.Count;native_passed=@($_.Group|Where-Object native_exit -eq 0).Count;guest_passed=@($_.Group|Where-Object guest_exit -eq 0).Count;exact_records=@($_.Group|Where-Object exact_records -eq $true).Count;strict_passed=@($_.Group|Where-Object strict -eq $true).Count;no_interface_leaks=@($_.Group|Where-Object live_interfaces -eq 0).Count}})
$aggregate|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $OutputDirectory 'aggregate.json');$aggregate|ConvertTo-Json
if(@($summary|Where-Object {$_.native_exit-ne 0 -or $_.runner_exit-ne 0 -or $_.guest_exit-ne 0 -or -not $_.exact_records -or -not $_.strict -or $_.live_interfaces-ne 0}).Count-ne 0){throw 'private Effect input gate failed; per-input evidence remains in target'}
