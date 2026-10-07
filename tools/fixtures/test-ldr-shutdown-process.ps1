<#
.SYNOPSIS
Compare authored returning loader shutdown on native Windows, interpreter and JIT.
.DESCRIPTION
Builds an EXE and two owned DLLs without a CRT. Checks once-only reverse DLL
detach, reserved flags, reentrant calls, current/peer FLS, peer survival, mapped
modules and LastError. Runner selects adv-run; OutputDirectory keeps binaries,
scalar output, reports and SHA-256 provenance inside target/investigations.
NativeOnly skips guest replay. No game or other host process is used.
.EXAMPLE
pwsh tools/fixtures/test-ldr-shutdown-process.ps1 -NativeOnly
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/ldr-shutdown-process'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'loader shutdown evidence must stay inside workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'ldr-shutdown-process.c'
foreach($kind in @('fixture','owned','second')){
    $definitions=@();if($kind-ne 'fixture'){$definitions+=@('/DOWNED_DLL')};if($kind-eq 'second'){$definitions+=@('/DMODULE_ID=2')}
    & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" @definitions $source "/Fo$nativeDirectory/$kind.obj"
    if($LASTEXITCODE-ne 0){throw 'loader shutdown fixture compile failed'}
}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'loader shutdown fixture link failed'}
foreach($kind in @('owned','second')){
    & (Join-Path $llvm 'lld-link.exe') /nologo /dll /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /entry:DllMain@12 /export:configure=_configure@4 "/out:$nativeDirectory/shutdown-$kind.dll" "$nativeDirectory/$kind.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw 'loader shutdown DLL link failed'}
}
@("Command: $fixture","Working directory: $nativeDirectory","Environment PATH: $env:PATH","Source SHA-256: $((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash)","Binary SHA-256: $((Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash)","DLL SHA-256: $((Get-FileHash -LiteralPath (Join-Path $nativeDirectory 'shutdown-owned.dll') -Algorithm SHA256).Hash)","Second DLL SHA-256: $((Get-FileHash -LiteralPath (Join-Path $nativeDirectory 'shutdown-second.dll') -Algorithm SHA256).Hash)")|Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native loader shutdown fixture timeout'};if($native.ExitCode-ne 0){throw "native loader shutdown fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'ldr-shutdown.bin'))
$expected=@(1,[uint32]0x13572468,2,1,1,1,259,258,1,1,[uint32]0x24681357,2,1,0,1,21,2,2,1,4,2,101,22,12,102)
if($reference.Length-ne $expected.Count*4){throw 'native loader shutdown results missing'}
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native loader shutdown invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output "native: $($expected.Count) returning loader-shutdown, reentrancy, FLS, DLL order, peer lifetime and LastError outcomes verified";exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    $runFixture=Join-Path $runDirectory 'fixture.exe'
    Copy-Item -LiteralPath $fixture -Destination $runFixture -Force
    foreach($kind in @('owned','second')){Copy-Item -LiteralPath (Join-Path $nativeDirectory "shutdown-$kind.dll") -Destination (Join-Path $runDirectory "shutdown-$kind.dll") -Force}
    $overlay=Join-Path $runDirectory 'data';$report=Join-Path $runDirectory 'report.json';$resultFile=Join-Path $overlay 'overlay/ldr-shutdown.bin'
    foreach($old in @($report,$resultFile)){if(Test-Path -LiteralPath $old){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("Command runner: $Runner","Arguments: $($arguments | ConvertTo-Json -Compress)","Working directory: $((Get-Location).Path)","Environment PATH: $env:PATH","Fixture SHA-256: $((Get-FileHash -LiteralPath $runFixture -Algorithm SHA256).Hash)","Runner SHA-256: $((Get-FileHash -LiteralPath $Runner -Algorithm SHA256).Hash)")|Set-Content -LiteralPath (Join-Path $runDirectory 'cmd.txt')
    & $Runner @arguments *> (Join-Path $runDirectory 'run.log')
    $runnerExit=$LASTEXITCODE
    $actual=[IO.File]::ReadAllBytes($resultFile)
    if($actual.Length-ne $reference.Length){throw "$variant missing loader shutdown results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native loader shutdown mismatch at DWORD $($i/4)"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($runnerExit-ne 0 -or $e.guest_exit_code-ne 0 -or $e.failure -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0){throw "$variant strict loader shutdown execution failed"}
    if($variant-eq 'jit' -and $e.native_entries-le 0){throw 'loader shutdown JIT fixture did not execute a translated region'}
    Write-Output "${variant}: all $($expected.Count) loader shutdown outcomes match native"
}
exit 0
