param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/nt-flush-cache'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'architecture-acceptance'+[IO.Path]::DirectorySeparatorChar)){throw 'instruction cache evidence must stay inside target/architecture-acceptance'}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'nt-flush-cache.c') "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'instruction cache fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'instruction cache fixture link failed'}
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $native.WaitForExit(10000)){throw 'native instruction cache fixture exceeded ten seconds'};if($native.ExitCode-ne 0){throw "native instruction cache fixture failed: $($native.ExitCode)"}}
finally{try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}finally{$native.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'nt-flush-cache.bin'))
if($reference.Length-ne 18*4 -or [BitConverter]::ToUInt32($reference,0)-ne 0 -or [BitConverter]::ToUInt32($reference,1*4)-ne 324478056){throw 'native instruction cache data check failed'}
if([BitConverter]::ToUInt32($reference,9*4)-ne 17 -or [BitConverter]::ToUInt32($reference,12*4)-ne 29){throw 'native changed-code execution failed'}
if($NativeOnly){Write-Output 'native: 18 DWORDs of instruction caches, counted ANSI names and LastError';exit 0}
foreach($variant in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$variant-data";$report=Join-Path $OutputDirectory "$variant.json"
    $arguments=@($fixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$variant.log")
    if($LASTEXITCODE-ne 0){throw "$variant instruction cache fixture failed"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/nt-flush-cache.bin'))
    if($actual.Length-ne $reference.Length){throw "$variant missing instruction cache results"}
    for($i=0;$i-lt $reference.Length;$i+=4){if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$variant native mismatch at DWORD $($i/4): $([BitConverter]::ToUInt32($actual,$i)) vs $([BitConverter]::ToUInt32($reference,$i))"}}
    $e=Get-Content -Raw $report|ConvertFrom-Json
    if($e.guest_exit_code-ne 0 -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0){throw "$variant strict instruction cache execution failed"}
    Write-Output "${variant}: instruction cache results match native"
}
