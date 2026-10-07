<#
.SYNOPSIS
Compare authored x86 processor-group and threadpool startup contracts.
.DESCRIPTION
Runner selects a frozen adv-run; OutputDirectory receives source-fixture builds,
native/guest scalar output, reports, command records and both executable hashes.
Processor groups use 29 invariants; waits/timers use 29; serial OpenMP uses eight.
The native OpenMP oracle uses the installed x86 vcomp140 runtime. No game is loaded.
NativeOnly verifies the native oracle without guest execution. Any mismatch fails.
.EXAMPLE
pwsh tools/fixtures/test-thread-startup.ps1 -Runner target/release/adv-run.exe
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/thread-startup/native-contracts'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10'
$version='10.0.26100.0'
foreach($spec in @(@('processor-groups','affinity.bin',29),@('threadpool-waits','waits.bin',29),@('vcomp-serial','vcomp.bin',8))) {
    $run=Join-Path $OutputDirectory $spec[0]
    $nativeDirectory=Join-Path $run 'native'
    New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
    $fixture=Join-Path $nativeDirectory 'fixture.exe'
    & "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot ($spec[0]+'.c')) "/Fo$nativeDirectory/fixture.obj"
    if($LASTEXITCODE-ne 0){throw 'fixture compile failed'}
    & "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw 'fixture link failed'}
    @('command=fixture.exe','environment=default',('guest_sha256='+(Get-FileHash $fixture).Hash),('runner_sha256='+(Get-FileHash $fixture).Hash)) | Set-Content (Join-Path $nativeDirectory 'cmd.txt')
    $native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
    try {
        if(-not $native.WaitForExit(20000)){throw 'native fixture exceeded twenty seconds'}
        if($native.ExitCode-ne 0){throw 'native fixture failed'}
    } finally {
        try {if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}} finally {$native.Dispose()}
    }
    $reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory $spec[1]))
    if($reference.Length-ne $spec[2]*4){throw 'native fixture result length mismatch'}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($reference,$i)-ne 1){throw "native invariant failed at index $($i/4)"}}
    Write-Output "$($spec[0]): $($spec[2]) native invariants passed"
    if($NativeOnly){continue}
    foreach($backend in @('interpreter','jit')) {
        $probe=Join-Path $run $backend
        New-Item -ItemType Directory -Force -Path $probe | Out-Null
        Copy-Item -LiteralPath $Runner -Destination (Join-Path $probe 'adv-run.exe')
        Get-ChildItem -LiteralPath (Split-Path $Runner) -Filter '*.dll' | Copy-Item -Destination $probe
        $binary=Join-Path $probe 'adv-run.exe'
        $arguments=@($fixture,'--headless','--backend',$backend,'--seconds','20','--data-dir',"$probe/data",'--report',"$probe/report.json",'--api-mode','strict')
        @($binary+' '+($arguments -join ' '),'environment=default',('guest_sha256='+(Get-FileHash $fixture).Hash),('runner_sha256='+(Get-FileHash $binary).Hash)) | Set-Content "$probe/cmd.txt"
        & $binary @arguments *> "$probe/run.log"
        if($LASTEXITCODE-ne 0){throw "$backend execution failed"}
        $actual=[IO.File]::ReadAllBytes((Join-Path "$probe/data/overlay" $spec[1]))
        if($actual.Length-ne $reference.Length){throw "$backend result length mismatch"}
        for($i=0;$i-lt $actual.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne 1){throw "$backend invariant failed at index $($i/4)"}}
        $report=Get-Content -Raw "$probe/report.json" | ConvertFrom-Json
        if($report.guest_exit_code-ne 0 -or $report.api.acceptance_eligible-ne $true -or $report.api.warnings.Count-ne 0){throw "$backend strict acceptance failed"}
        Write-Output "$($spec[0]): $backend matches $($spec[2]) native invariants"
    }
}
