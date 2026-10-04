param([string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),[string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/crt-exit-convert'),[switch]$NativeOnly)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$nativeDir=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-crt-exit-convert.ps1') -OutputDirectory $nativeDir
foreach($variant in @('native','apisets')){
    $binary=Join-Path $nativeDir "fixture_crt_$variant.exe"
    $data=Join-Path $nativeDir 'crt-exit-convert.bin'
    if(Test-Path -LiteralPath $data){Remove-Item -LiteralPath $data -Force}
    $process=Start-Process -FilePath $binary -WorkingDirectory $nativeDir -WindowStyle Hidden -PassThru
    try {if(-not $process.WaitForExit(5000)){throw 'owned CRT fixture timeout'};if($process.ExitCode-ne 0){throw "native CRT fixture failed: $($process.ExitCode)"}}
    finally{if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null};$process.Dispose()}
    $reference=[IO.File]::ReadAllBytes($data)
    [IO.File]::WriteAllBytes((Join-Path $nativeDir "$variant.bin"),$reference)
    if($reference.Length-lt 700 -or [BitConverter]::ToUInt32($reference,$reference.Length-4)-ne 0xcab1e123u){throw 'incomplete native CRT evidence'}
    Write-Output "native $variant`: $($reference.Length/4) DWORDs"
    if($NativeOnly){continue}
    foreach($backend in @('interpreter','jit')){
        $folder=Join-Path $OutputDirectory "$variant-$backend"
        $report=Join-Path $folder 'report.json'
        New-Item -ItemType Directory -Force $folder | Out-Null
        & $Runner $binary --headless --backend $backend --seconds 5 --data-dir (Join-Path $folder 'data') --report $report *> (Join-Path $folder 'run.log')
        if($LASTEXITCODE-ne 0){throw "CRT $variant/$backend failed: $folder/run.log"}
        $actual=[IO.File]::ReadAllBytes((Join-Path $folder 'data/overlay/crt-exit-convert.bin'))
        if([Convert]::ToBase64String($reference)-cne [Convert]::ToBase64String($actual)){throw "CRT $variant/$backend differs from native"}
        $result=Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
        if($result.api.acceptance_eligible-ne $true -or $result.guest_exit_code-ne 0){throw 'CRT strict acceptance failed'}
        Write-Output "$variant/$backend`: $($actual.Length/4) exact DWORDs"
    }
}
