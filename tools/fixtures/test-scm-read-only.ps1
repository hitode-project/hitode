<#
.SYNOPSIS
Compare authored read-only SCM parameter and access contracts across backends.
.DESCRIPTION
Usage: test-scm-read-only.ps1 -OutputDirectory <investigation-directory>
       [-NativeOnly] [-ReuseNative] [-Runner <adv-run.exe>].
Input: LLVM, the Windows SDK, and optionally a current guest runner. Output:
124 scalar DWORD outcomes, reports, logs and cmd.txt with environment and two
SHA-256 hashes. All generated files stay beneath the primary repository's
target/investigations, even when this script belongs to a separate worktree.
Checks SCM opens, access checks, invalid names, required output pointers and
close semantics. Native catalog enumeration and one accessible configuration
produce only counts and result codes. Host names and configuration contents
are never saved, and variable native catalog counts are not guest oracles.
Sixty-four repeated absent-group calls verify stable error/output contracts
while exercising translated API paths; they do not add output records.
The guest default profile must enumerate its valid empty catalog successfully.
NativeOnly runs the built-in scalar contract checks without guest execution.
ReuseNative accepts a saved native reference only when both source and fixture
hashes match its provenance, avoiding repeated host enumeration and builds.
#>
param(
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [string]$Runner='',
    [string]$LlvmBin='C:\Program Files\LLVM\bin',
    [string]$WindowsSdk='C:\Program Files (x86)\Windows Kits\10',
    [string]$SdkVersion='10.0.26100.0',
    [switch]$NativeOnly,
    [switch]$ReuseNative
)
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$common=(& git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir)
if($LASTEXITCODE-ne 0){throw 'SCM fixture must run from its Git worktree'}
$primary=Split-Path -Parent ([IO.Path]::GetFullPath($common))
$allowed=[IO.Path]::GetFullPath((Join-Path $primary 'target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
    throw 'SCM evidence must stay beneath the primary repository target/investigations'
}
if(-not $NativeOnly){
    if(-not $Runner){throw 'Runner is required for guest differential execution'}
    $Runner=[IO.Path]::GetFullPath($Runner)
    if(-not (Test-Path -LiteralPath $Runner -PathType Leaf)){throw 'SCM guest runner is missing'}
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory|Out-Null
$source=Join-Path $PSScriptRoot 'scm-read-only.c'
$fixture=Join-Path $nativeDirectory 'fixture.exe'
$compile=@('/nologo','/c','/O2','/GS-','/Gs9999999','/Zl','/clang:-fno-builtin','/clang:--target=i686-pc-windows-msvc',"/I$WindowsSdk/Include/$SdkVersion/um","/I$WindowsSdk/Include/$SdkVersion/shared","/I$WindowsSdk/Include/$SdkVersion/ucrt",$source,"/Fo$nativeDirectory/fixture.obj")
$link=@('/nologo','/machine:x86','/nodefaultlib','/dynamicbase','/base:0x400000','/safeseh:no','/timestamp:0',"/libpath:$WindowsSdk/Lib/$SdkVersion/um/x86",'/subsystem:windows','/entry:mainCRTStartup',"/out:$fixture","$nativeDirectory/fixture.obj",'kernel32.lib','advapi32.lib')
if($ReuseNative){
    $provenance=Get-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt') -Raw
    foreach($inputFile in @(@($source,'source'),@($fixture,'fixture'))){
        $expected="$($inputFile[1])_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $inputFile[0]).Hash)"
        if(-not $provenance.Contains($expected)){throw 'saved SCM native source/fixture hash does not match'}
    }
}else{
    & (Join-Path $LlvmBin 'clang-cl.exe') @compile
    if($LASTEXITCODE-ne 0){throw 'SCM fixture compilation failed'}
    & (Join-Path $LlvmBin 'lld-link.exe') @link
    if($LASTEXITCODE-ne 0){throw 'SCM fixture linking failed'}
}

function Invoke-ScmProbe {
    param([string]$Executable,[string[]]$Arguments=@(),[string]$Directory,[int]$TimeoutMs=10000)
    # Start-Process joins ArgumentList; quote each Windows argument separately.
    $quoted=@($Arguments|ForEach-Object {'"'+([regex]::Replace([regex]::Replace($_,'(\\*)"','$1$1\"'),'(\\+)$','$1$1'))+'"'})
    $options=@{FilePath=$Executable;WorkingDirectory=$Directory;WindowStyle='Hidden';PassThru=$true;RedirectStandardOutput=(Join-Path $Directory 'run.log');RedirectStandardError=(Join-Path $Directory 'stderr.log')}
    if($quoted.Count){$options.ArgumentList=$quoted}
    $process=Start-Process @options
    try{
        if(-not $process.WaitForExit($TimeoutMs)){throw 'SCM fixture exceeded its bounded runtime'}
        return $process.ExitCode
    }finally{
        try{if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null}}
        finally{$process.Dispose()}
    }
}
function Read-ScmWords {
    param([string]$Path)
    $bytes=[IO.File]::ReadAllBytes($Path)
    if($bytes.Length-ne 124*4){throw 'SCM scalar output has the wrong length'}
    return @(for($index=0;$index-lt 124;$index++){[BitConverter]::ToUInt32($bytes,$index*4)})
}
function Assert-ScmWords {
    param([uint32[]]$Words,[uint32[]]$Expected,[int]$Offset,[string]$Label)
    for($index=0;$index-lt $Expected.Count;$index++){
        if($Words[$Offset+$index]-ne $Expected[$index]){throw "$Label mismatch at DWORD $($Offset+$index): actual=$($Words[$Offset+$index]) expected=$($Expected[$index])"}
    }
}
$environment=@("environment PATH: $env:PATH","environment TEMP: $env:TEMP","environment TMP: $env:TMP")+@(Get-ChildItem Env:|Where-Object {$_.Name-like 'ADV_*'}|Sort-Object Name|ForEach-Object {"environment $($_.Name): $($_.Value)"})
$resultName='scm-read-only.bin'
$nativeResult=Join-Path $nativeDirectory $resultName
if(-not $ReuseNative){
    @("compile: clang-cl $($compile-join ' ')","link: lld-link $($link-join ' ')",'command: fixture.exe (working directory: native)')+$environment+@("source_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $fixture).Hash)")|Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
    if(Test-Path -LiteralPath $nativeResult -PathType Leaf){Remove-Item -LiteralPath $nativeResult}
    $nativeExit=Invoke-ScmProbe -Executable $fixture -Directory $nativeDirectory
    if($nativeExit-ne 0){throw "native SCM fixture failed: $nativeExit"}
}
$reference=Read-ScmWords $nativeResult
$marker=[uint32]0x13572468
$untouchedNeeded=[uint32]2863311530
$untouchedCount=[uint32]3149642683
Assert-ScmWords $reference @(1,0,1,0,1,0,0,123,0,123,0,5,1,0) 0 'native manager opens'
$enums=@(
    @(0,1060,0,0,0),@(0,1060,0,0,0),@(0,124,$untouchedNeeded,$untouchedCount,0),
    @(0,87,0,0,0),@(0,87,0,0,0),@(0,5,0,0,0),@(0,6,$untouchedNeeded,$untouchedCount,0),
    @(0,1060,0,0,0),@(0,487,$untouchedNeeded,$untouchedCount,0),
    @(0,487,$untouchedNeeded,$untouchedCount,0),@(0,1060,0,0,0),@(0,1060,0,0,0)
)
for($case=0;$case-lt 12;$case++){Assert-ScmWords $reference $enums[$case] (14+$case*5) 'native enumeration'}
Assert-ScmWords $reference @(0,1060,0,123,0,123,0,6,0,487,0,6) 74 'native service opens and wrong kind'
Assert-ScmWords $reference @(1,$marker,0,6,0,6) 118 'native close semantics'
for($case=0;$case-lt 3;$case++){
    $offset=86+$case*8
    Assert-ScmWords $reference @(1,$marker) $offset 'native catalog success'
    if($reference[$offset+2]-ne $reference[$offset+3]+$reference[$offset+4]+$reference[$offset+5]){throw 'native catalog counts do not partition the returned rows'}
    if($case-lt 2 -and $reference[$offset+5]-ne 0){throw 'native Win32 filter returned a non-Win32 row'}
    Write-Output "native filter=$case total=$($reference[$offset+2]) interactive=$($reference[$offset+3]) ordinary=$($reference[$offset+4]) other=$($reference[$offset+5]) user=$($reference[$offset+6]) instance=$($reference[$offset+7])"
}
if($reference[110]-eq 1){
    if($reference[111]-ne 1 -or $reference[114]-ne 0 -or $reference[115]-ne 122 -or $reference[116]-lt 36 -or $reference[117]-ne 1){throw 'native accessible configuration contract failed'}
    Write-Output "native configuration success_error=$($reference[112]) success_needed=$($reference[113]) short_error=$($reference[115]) short_needed=$($reference[116])"
}
Write-Output 'native: 92 shared SCM outcomes checked; catalog observations contain only scalar statistics'
if($NativeOnly){exit 0}

$bin=Join-Path $OutputDirectory 'bin'
New-Item -ItemType Directory -Force -Path $bin|Out-Null
$runFixture=Join-Path $bin 'fixture.exe'
$runRunner=Join-Path $bin 'adv-run.exe'
Copy-Item -LiteralPath $fixture -Destination $runFixture -Force
if(-not $Runner.Equals([IO.Path]::GetFullPath($runRunner),[StringComparison]::OrdinalIgnoreCase)){
    Copy-Item -LiteralPath $Runner -Destination $runRunner -Force
}
# Both backends share one frozen executable set. Link only media dependencies
# present alongside the input runner, retaining the same-volume files once.
$runnerDirectory=Split-Path -Parent $Runner
foreach($pattern in @('avcodec-*.dll','avformat-*.dll','avutil-*.dll','swresample-*.dll','swscale-*.dll')){
    foreach($dependency in (Get-ChildItem -LiteralPath $runnerDirectory -Filter $pattern -File)){
        $destination=Join-Path $bin $dependency.Name
        if(Test-Path -LiteralPath $destination -PathType Leaf){
            if((Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash-ne (Get-FileHash -Algorithm SHA256 -LiteralPath $dependency.FullName).Hash){throw 'frozen SCM media dependency differs from the input runner'}
        }else{
            New-Item -ItemType HardLink -Path $destination -Target $dependency.FullName|Out-Null
        }
    }
}
foreach($variant in @('interpreter','jit')){
    $runDirectory=Join-Path $OutputDirectory $variant
    New-Item -ItemType Directory -Force -Path $runDirectory|Out-Null
    $overlay=Join-Path $runDirectory 'data'
    $report=Join-Path $runDirectory 'report.json'
    $actualPath=Join-Path $overlay "overlay/$resultName"
    foreach($old in @($report,$actualPath)){if(Test-Path -LiteralPath $old -PathType Leaf){Remove-Item -LiteralPath $old}}
    $arguments=@($runFixture,'--hidden','--backend',$variant,'--seconds','10','--data-dir',$overlay,'--report',$report,'--trace',(Join-Path $runDirectory 'api.log'),'--api-mode','strict')
    if($variant-eq 'jit'){$arguments+='--sync-compilation'}
    @("command: adv-run $($arguments-join ' ')")+$environment+@("runner_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $runRunner).Hash)","fixture_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $runFixture).Hash)")|Set-Content -LiteralPath (Join-Path $runDirectory 'cmd.txt')
    $guestExit=Invoke-ScmProbe -Executable $runRunner -Arguments $arguments -Directory $runDirectory -TimeoutMs 20000
    if($guestExit-ne 0){throw "$variant SCM runner failed: $guestExit"}
    $actual=Read-ScmWords $actualPath
    foreach($index in (@(0..85)+@(118..123))){if($actual[$index]-ne $reference[$index]){throw "$variant SCM native mismatch at DWORD $index"}}
    for($case=0;$case-lt 3;$case++){Assert-ScmWords $actual @(1,$marker,0,0,0,0,0,0) (86+$case*8) "$variant empty catalog"}
    Assert-ScmWords $actual @(0,0,0,0,0,0,0,0) 110 "$variant absent configuration"
    $e=Get-Content -LiteralPath $report -Raw|ConvertFrom-Json
    if($e.backend-ne $variant -or $e.stop_reason-ne 'guest_exit' -or $e.guest_exit_code-ne 0 -or $e.failure -or $e.api.acceptance_eligible-ne $true -or $e.api.warnings.Count-ne 0 -or $e.api.stub_calls-ne 0 -or @($e.api.called_gaps.PSObject.Properties).Count-ne 0){throw "$variant strict SCM report failed"}
    if($variant-eq 'jit' -and $e.native_entries-le 0){throw 'SCM JIT fixture did not execute translated code'}
    Write-Output "${variant}: 92 shared outcomes match native; virtual empty-catalog and rights contracts pass"
}
exit 0
