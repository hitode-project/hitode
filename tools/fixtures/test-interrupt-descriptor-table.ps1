<#
.SYNOPSIS
Compare authored SIDT behavior on native Windows, interpreter and JIT.
.DESCRIPTION
Runner selects adv-run; OutputDirectory stores authored binaries, scalar results,
logs and cmd.txt evidence inside target/investigations. NativeOnly skips guest
replays. Checks both operand sizes, repeated queries, flags, registers, LastError
and six-byte writes with canaries. Native IDTR addresses are never output.
.EXAMPLE
pwsh tools/fixtures/test-interrupt-descriptor-table.ps1 -NativeOnly
#>
param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/interrupt-descriptor-table'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'investigations'+[IO.Path]::DirectorySeparatorChar)){
    throw 'interrupt descriptor evidence must stay inside target/investigations'
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10'
$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'interrupt-descriptor-table.c'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'interrupt descriptor fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'interrupt descriptor fixture link failed'}
@(
    'command: fixture.exe (authored SIDT fixture; ten-second timeout)',
    'environment: native Windows; no sample input',
    "source_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash)",
    "fixture_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $fixture).Hash)"
) | Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{
    if(-not $native.WaitForExit(10000)){throw 'native interrupt descriptor fixture exceeded ten seconds'}
    if($native.ExitCode-ne 0){throw "native interrupt descriptor fixture failed: $($native.ExitCode)"}
}finally{
    try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}
    finally{$native.Dispose()}
}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'interrupt-descriptor-table.bin'))
if($reference.Length-ne 8*4){throw 'native interrupt descriptor results missing'}
for($i=0;$i-lt 8;$i++){
    if([BitConverter]::ToUInt32($reference,$i*4)-ne 1){throw "native interrupt descriptor invariant failed at DWORD $i"}
}
if($NativeOnly){Write-Output 'native: eight SIDT invariants pass; no descriptor addresses output';exit 0}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    $copy=Join-Path $runDirectory 'fixture.exe'
    Copy-Item -LiteralPath $fixture -Destination $copy
    $overlay=Join-Path $runDirectory 'data'
    $report=Join-Path $runDirectory 'report.json'
    $arguments=@($copy,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @(
        "command: adv-run $($arguments -join ' ')",
        'environment: inherited host environment; no sample input',
        "runner_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $Runner).Hash)",
        "fixture_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $copy).Hash)"
    ) | Set-Content -LiteralPath (Join-Path $runDirectory 'cmd.txt')
    & $Runner @arguments *> (Join-Path $runDirectory 'run.log')
    if($LASTEXITCODE-ne 0){throw "$variant interrupt descriptor fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/interrupt-descriptor-table.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing interrupt descriptor results"}
    for($i=0;$i-lt $reference.Length;$i+=4){
        if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){
            throw "$variant native mismatch at DWORD $($i/4)"
        }
    }
    $e=Get-Content -Raw -LiteralPath $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){
        throw "$variant strict interrupt descriptor execution failed"
    }
    Write-Output "${variant}: eight SIDT invariants match native"
}
