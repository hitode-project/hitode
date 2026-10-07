<#
.SYNOPSIS
Compare authored SLDT operand behavior on native Windows, interpreter and JIT.
.DESCRIPTION
Runner selects adv-run; OutputDirectory stores authored binaries and eight scalar
outcomes under target/architecture-acceptance. NativeOnly skips guest replays.
Checks 16/32-bit register results, preserved flags and registers, LastError and
a two-byte memory write with canaries. No sample runs.
.EXAMPLE
pwsh tools/fixtures/test-local-descriptor-selector.ps1 -NativeOnly
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/local-descriptor-selector'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'local descriptor evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'local-descriptor-selector.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'local descriptor fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'local descriptor fixture link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native local descriptor fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native local descriptor fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'local-descriptor-selector.bin'))
$expected=@(0,[uint32]2864381952,1,0,1,[uint32]0x13572468,[uint32]0x13572468,1)
if($reference.Length-ne 8*4){throw 'native local descriptor results missing'}
for($i=0;$i-lt $expected.Count;$i++){if([BitConverter]::ToUInt32($reference,$i*4)-ne $expected[$i]){throw "native local descriptor invariant failed at DWORD $i"}}
if($NativeOnly){Write-Output 'native: eight DWORDs of SLDT widths, flags and owned memory';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant local descriptor fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/local-descriptor-selector.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing local descriptor results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4)"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict local descriptor execution failed"}
    Write-Output "${variant}: local descriptor results match native"
}
