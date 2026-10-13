<#
Check x86 FPU controls across D3D9 creation, Reset, Present, and Release, plus guest refresh pacing.
Inputs: authored d3d9-environment.c variants, Windows SDK, LLVM, and an adv-run GPU build.
Build: cargo build -p adv-run --features adv-shell/gpu
Run: pwsh -NoProfile -File tools/fixtures/test-d3d9-environment.ps1
Use -GuestVisible, -HoldControl, and -NoTimeSkip to select pacing cases.
Native execution remains hidden; guest visibility is logical inside a hidden shell.
Outputs: d3d9-environment.bin under target/fixtures and per-variant reports, logs, and overlays.
-OutputDirectory defaults to target/architecture-acceptance/d3d9-environment.
#>
param(
    [string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/architecture-acceptance/d3d9-environment'),
    [switch]$GuestVisible,
    [switch]$HoldControl,
    [switch]$NoTimeSkip
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$acceptanceRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if($OutputDirectory -ne $acceptanceRoot -and -not $OutputDirectory.StartsWith($acceptanceRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
    throw 'D3D9 evidence directory must be inside target/architecture-acceptance'
}
$fixtureDirectory=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/fixtures'))
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw "output escaped directory: $path"}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $PSScriptRoot 'build-d3d9-environment.ps1') -OutputDirectory $fixtureDirectory
Remove-FixtureOutput $fixtureDirectory 'd3d9-environment.bin'
$fixture=Join-Path $fixtureDirectory 'fixture_d3d9_environment.exe'
$native=Start-Process -FilePath $fixture -WorkingDirectory $fixtureDirectory -WindowStyle Hidden -PassThru
try {
    if(-not $native.WaitForExit(5000)){throw 'native environment fixture timed out'}
    if($native.ExitCode -ne 0){throw "native fixture failed: $($native.ExitCode)"}
} finally {
    if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}
    $native.Dispose()
}
$reference=[IO.File]::ReadAllBytes((Join-Path $fixtureDirectory 'd3d9-environment.bin'))
if($reference.Length -ne 82*4){throw 'native environment output size mismatch'}
if($GuestVisible){
    # This variant is run only inside the hidden emulator shell. The native
    # oracle above remains hidden and never opens a visible desktop window.
    & (Join-Path $PSScriptRoot 'build-d3d9-environment.ps1') -OutputDirectory $fixtureDirectory -GuestVisible
    $fixture=Join-Path $fixtureDirectory 'fixture_d3d9_environment_visible.exe'
}
# Independent expectations for the controls, with and without FPU_PRESERVE.
for($case=0;$case -lt 5;$case++){
    $before=[BitConverter]::ToUInt32($reference,$case*64)
    $expected=if(($case -band 1) -ne 0){$before}else{($before -band 0xf0c0) -bor 0x3f}
    foreach($word in @(3,8,11,14)){
        if([BitConverter]::ToUInt32($reference,($case*16+$word)*4) -ne $expected){throw "native case $case unexpected x87 control"}
    }
}
foreach($variant in @('interpreter','jit','software-interpreter','software-jit')){
    $backend=if($variant.EndsWith('jit')){'jit'}else{'interpreter'}
    $overlay=Join-Path $OutputDirectory "$variant-data"
    $report=Join-Path $OutputDirectory "$variant.json"
    foreach($name in @("$variant.json","$variant.log")){Remove-FixtureOutput $OutputDirectory $name}
    Remove-FixtureOutput $overlay 'overlay/d3d9-environment.bin'
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    if($variant.StartsWith('software')){$arguments+='--gpu-software'}
    if($NoTimeSkip){$arguments+='--no-time-skip'}
    if($HoldControl){
        $arguments+='--control-stdin'
        'key-down 0x11' | & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    }else{& $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")}
    if($LASTEXITCODE -ne 0){throw "$variant failed; see $OutputDirectory/$variant.log"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/d3d9-environment.bin'))
    if($actual.Length -ne $reference.Length){throw "$variant missing environment words"}
    $stickyDifferences=0
    for($word=0;$word -lt 82;$word++){
        $expected=[BitConverter]::ToUInt32($reference,$word*4)
        $got=[BitConverter]::ToUInt32($actual,$word*4)
        if($GuestVisible -and $word -lt 80 -and ($word%16) -eq 10){$expected=0}
        if($word -lt 80 -and ($word%16) -in @(4,9,12,15)){
            # Driver-internal arithmetic marks native MXCSR precision sticky even
            # with FPU_PRESERVE. Only that observed status bit may differ; never
            # manufacture it in the guest. Guest keeps all previously set flags.
            $initial=[BitConverter]::ToUInt32($actual,([int][Math]::Floor($word/16)*16+1)*4)
            if($got -ne $initial -or ($got -bor 0x20) -ne ($expected -bor 0x20)){throw "$variant unexpected MXCSR state at word $word"}
            if($got -ne $expected){$stickyDifferences++}
        }elseif($got -ne $expected){throw "$variant native mismatch at word $word ($got vs $expected)"}
    }
    $evidence=Get-Content -Raw $report | ConvertFrom-Json
    if($evidence.guest_exit_code -ne 0 -or $evidence.frames -ne 5 -or $evidence.api.acceptance_eligible -ne $true){throw "$variant strict execution failed"}
    if($evidence.gpu.live_interfaces -ne 0 -or $evidence.gpu.pending_waits -ne 0 -or $evidence.gpu.pending_readbacks -ne 0){throw "$variant COM/wait leaked"}
    if($evidence.gpu.commands.submissions -ne 1 -or $evidence.gpu.commands.readbacks -ne 1 -or $evidence.gpu.commands.snapshot_allocations -ne 0){throw "$variant Present added GPU copies/submits"}
    if($evidence.time_skip.jumps -ne 0){throw "$variant Present boost must not jump virtual time"}
    if($GuestVisible -and -not $HoldControl -and $evidence.idle_waits -lt 1){throw "$variant logical visible refresh did not wait"}
    if($GuestVisible -and $HoldControl -and $evidence.idle_waits -ne 0){throw "$variant held Ctrl Present unexpectedly waited"}
    if($backend -eq 'jit' -and $evidence.native_entries -eq 0){throw "$variant no compiled guest entries"}
    Write-Output "${variant}: 82 oracle words checked; x87/MXCSR controls matched, $stickyDifferences documented native precision-flag differences; entries=$($evidence.native_entries), refresh time skips=$($evidence.time_skip.jumps), idle waits=$($evidence.idle_waits)"
}
