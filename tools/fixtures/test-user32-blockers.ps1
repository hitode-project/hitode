param([string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),[string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/user32-blockers'),[switch]$NativeOnly)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'USER32 evidence must stay inside target/architecture-acceptance'}
$fixturePath=Join-Path $OutputDirectory 'fixture_user32_blockers.exe'
& (Join-Path $PSScriptRoot 'build-user32-blockers.ps1') -OutputDirectory $OutputDirectory
$outputPath=Join-Path $OutputDirectory 'user32-blockers.bin'
if(Test-Path -LiteralPath $outputPath){Remove-Item -LiteralPath $outputPath -Force}
$ownedProbe=Start-Process -FilePath $fixturePath -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
try{if(-not $ownedProbe.WaitForExit(5000)){throw 'owned USER32 probe exceeded five seconds'};if($ownedProbe.ExitCode-ne 0){throw "owned USER32 probe failed: $($ownedProbe.ExitCode)"}}
finally{try{if(-not $ownedProbe.HasExited){$ownedProbe.Kill();$ownedProbe.WaitForExit(1000)|Out-Null}}finally{$ownedProbe.Dispose()}}
$bytes=[IO.File]::ReadAllBytes($outputPath)
if($bytes.Length-ne 360 -or [BitConverter]::ToUInt32($bytes,356)-ne [Convert]::ToUInt32('B10C0001',16)){throw 'incomplete USER32 native evidence'}
[IO.File]::WriteAllBytes((Join-Path $OutputDirectory 'native.bin'),$bytes)
Write-Output 'native: 90 DWORDs, including 16 BGRA32 and 8 RGB555/RGB565 pixels'
if($NativeOnly){return}
$virtualFace='MS UI Gothic'
foreach($backend in @('interpreter','jit')){
    $folder=Join-Path $OutputDirectory $backend
    $overlay=Join-Path $folder 'data/overlay/user32-blockers.bin'
    New-Item -ItemType Directory -Force $folder | Out-Null
    if(Test-Path -LiteralPath $overlay){Remove-Item -LiteralPath $overlay -Force}
    $report=Join-Path $folder 'report.json'
    & $Runner $fixturePath --headless --backend $backend --seconds 5 --data-dir (Join-Path $folder 'data') --report $report --trace (Join-Path $folder 'api.log') *> (Join-Path $folder 'run.log')
    if($LASTEXITCODE-ne 0){throw "USER32 $backend runner failed: $folder/run.log"}
    $actual=[IO.File]::ReadAllBytes($overlay)
    [IO.File]::WriteAllBytes((Join-Path $folder 'guest.bin'),$actual)
    if($actual.Length-ne $bytes.Length){throw 'incomplete guest USER32 evidence'}
    for($index=0;$index-lt 90;++$index){
        $observed=[BitConverter]::ToUInt32($actual,4*$index)
        if($index-ge 25 -and $index-le 56){
            # The guest UI profile is isolated from the host desktop font.
            $position=$index-25
            $expected=if($position-lt $virtualFace.Length){[uint32][char]$virtualFace[$position]}else{[uint32]0}
        }else{$expected=[BitConverter]::ToUInt32($bytes,4*$index)}
        if($observed-ne $expected){throw ('USER32 {0} word {1}: {2:X8}, expected {3:X8}' -f $backend,$index,$observed,$expected)}
    }
    $result=Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
    if($result.api.acceptance_eligible-ne $true -or $result.guest_exit_code-ne 0){throw 'USER32 strict acceptance failed'}
    Write-Output "$backend`: 58 exact native words + 32 explicit isolated-profile face words; 24 exact pixels"
}
