<#
.SYNOPSIS
Compare an authored heap-lock fixture on native Windows, interpreter and JIT.
.DESCRIPTION
Runner selects adv-run. OutputDirectory keeps fixture binaries, scalar results,
reports, logs and cmd.txt provenance under this workspace's target/investigations.
NativeOnly skips guest replay. Checks recursive
RTL/Win32 ownership, serialized allocation/free, NO_SERIALIZE and LastError.
Only BOOLEAN AL is compared; EAX upper bits are advisory. No sample is loaded.
.EXAMPLE
pwsh tools/fixtures/test-heap-lock.ps1 -NativeOnly
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/heap-lock'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'heap-lock evidence must stay inside workspace target/investigations'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10'
$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'heap-lock.c'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'heap-lock fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'heap-lock fixture link failed'}
@("Command: $fixture","Working directory: $nativeDirectory","Environment PATH: $env:PATH","Source SHA-256: $((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash)","Binary SHA-256: $((Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash)") | Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native heap-lock fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native heap-lock fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'heap-lock.bin'))
$sentinel=[uint32]0x13572468
$expected=@(1,1,1,$sentinel,1,$sentinel,1,$sentinel,1,1,1,1,1,$sentinel,1,1)
foreach($mode in 0..7){$timeout=if(($mode-band 2)-eq 0){258}else{0};$expected+=@(0,$timeout,1,$timeout,1,0,0,1,$sentinel,1,$sentinel)}
$expected+=1
if($reference.Length-ne 107*4){throw 'native heap-lock results missing'}
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native heap-lock invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output 'native: 105 scalar heap-lock recursion, Win32/RTL interop, serialization, NO_SERIALIZE and LastError outcomes verified';exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    $runFixture=Join-Path $runDirectory 'fixture.exe'
    Copy-Item -LiteralPath $fixture -Destination $runFixture -Force
    $overlay=Join-Path $runDirectory 'data'
    $report=Join-Path $runDirectory 'report.json'
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("Command runner: $Runner","Arguments: $($arguments | ConvertTo-Json -Compress)","Working directory: $((Get-Location).Path)","Environment PATH: $env:PATH","Fixture SHA-256: $((Get-FileHash -LiteralPath $runFixture -Algorithm SHA256).Hash)","Runner SHA-256: $((Get-FileHash -LiteralPath $Runner -Algorithm SHA256).Hash)") | Set-Content -LiteralPath (Join-Path $runDirectory 'cmd.txt')
    & $Runner @arguments *> (Join-Path $runDirectory 'run.log')
    if($LASTEXITCODE-ne 0){throw "$variant heap-lock fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/heap-lock.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing heap-lock results"}
    for($i=0;$i-lt $expected.Count*4;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native heap-lock mismatch at DWORD $($i/4)"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict heap-lock execution failed"}
    Write-Output "${variant}: all 105 heap-lock outcomes match native"
}
exit 0
