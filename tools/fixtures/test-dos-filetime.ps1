param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/dos-filetime-tests'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$Runner=[IO.Path]::GetFullPath($Runner)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$targetRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($targetRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'DOS evidence must stay inside workspace target'}
function Remove-FixtureOutput([string]$directory,[string]$name){
    $path=[IO.Path]::GetFullPath((Join-Path $directory $name))
    if(-not $path.StartsWith($directory+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'output escaped directory'}
    if(Test-Path -LiteralPath $path){Remove-Item -LiteralPath $path -Force}
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory|Out-Null
$llvmBin='C:/Program Files/LLVM/bin'
$sdkLib='C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x86'
& "$llvmBin/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'dos_filetime.c') "/Fo$nativeDirectory/dos_filetime.obj"
if($LASTEXITCODE-ne 0){throw 'authored DOS fixture compile failed'}
& "$llvmBin/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 /subsystem:console /entry:mainCRTStartup "/libpath:$sdkLib" "/out:$nativeDirectory/fixture_dos_filetime.exe" "$nativeDirectory/dos_filetime.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'authored DOS fixture link failed'}
Remove-FixtureOutput $nativeDirectory 'dos-filetime.bin'
$fixture=Join-Path $nativeDirectory 'fixture_dos_filetime.exe'
$child=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{if(-not $child.WaitForExit(5000)){throw 'authored native DOS fixture exceeded five seconds'};if($child.ExitCode-ne 0){throw "native DOS fixture exit $($child.ExitCode)"}}
finally{try{if(-not $child.HasExited){$child.Kill();$child.WaitForExit(1000)|Out-Null}}finally{$child.Dispose()}}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'dos-filetime.bin'))
if($reference.Length-ne 277*4 -or [BitConverter]::ToUInt32($reference,$reference.Length-4)-ne 0xd05f17e0u){throw 'native DOS evidence incomplete'}
Write-Output 'native: 25 FILETIME + 21 DOS cases, 277 DWORDs'
if($NativeOnly){return}
foreach($backend in @('interpreter','jit')){
    $overlay=Join-Path $OutputDirectory "$backend-data"
    $report=Join-Path $OutputDirectory "$backend.json"
    Remove-FixtureOutput $overlay 'overlay/dos-filetime.bin'
    Remove-FixtureOutput $OutputDirectory "$backend.json"
    $arguments=@($fixture,'--hidden','--backend',$backend,'--seconds','6','--data-dir',$overlay,'--report',$report)
    # This tiny deterministic fixture can finish before an async compilation
    # publishes. Compile synchronously only here to exercise native guest code.
    if($backend-eq 'jit'){$arguments+='--sync-compilation'}
    & $Runner @arguments *> (Join-Path $OutputDirectory "$backend.log")
    if($LASTEXITCODE-ne 0){throw "$backend DOS fixture failed: $OutputDirectory/$backend.log"}
    $actual=[IO.File]::ReadAllBytes((Join-Path $overlay 'overlay/dos-filetime.bin'))
    if([Convert]::ToBase64String($actual)-cne [Convert]::ToBase64String($reference)){
        for($i=0;$i-lt [Math]::Min($actual.Length,$reference.Length);$i+=4){
            if([BitConverter]::ToUInt32($actual,$i)-ne [BitConverter]::ToUInt32($reference,$i)){throw "$backend native mismatch at DWORD $($i/4)"}
        }
        throw "$backend DOS evidence length mismatch"
    }
    $evidence=Get-Content -Raw $report|ConvertFrom-Json
    if($evidence.guest_exit_code-ne 0 -or $evidence.api.acceptance_eligible-ne $true -or $evidence.api.stub_calls-ne 0 -or $evidence.frames-ne 0){throw "$backend strict DOS execution failed"}
    if($backend-eq 'jit' -and $evidence.native_entries-eq 0){throw 'DOS fixture did not execute JIT code'}
    Write-Output "${backend}: 277 exact native DWORDs; typed APIs, no stubs; entries=$($evidence.native_entries)"
}
