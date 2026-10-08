<#
.SYNOPSIS
Compare suspended initial-EIP redirection, TLS ordering, and context preservation.
.DESCRIPTION
Usage: test-initial-eip-startup.ps1 -OutputDirectory <target/investigations/run>
       [-NativeOnly] [-ReuseNative] [-Runner <adv-run.exe>]
       [-Backends interpreter,jit] [-ExpectBootstrapGap].
       test-initial-eip-startup.ps1 -SelfTest
Input: the adjacent authored C source, LLVM, Windows SDK, and optionally a runner.
Output: parent46/child20 scalar DWORD files, comparison.json, reports and cmd.txt
with source, parent, child, and runner SHA-256. No game or protected DLL runs.
The parent starts normal and EIP-redirected owned children while suspended. The
TLS callback changes volatile GP, arithmetic flags, x87 CW, MXCSR and XMM0. The
naked continuation captures state before a compiler prologue. Native preserves
GP/control/flags across startup, while initialization FP effects remain active.
XMM0 after native system-loader code is observed, not an exact guest oracle.
ExpectBootstrapGap accepts only the known old-runner missing-TLS behavior, for
a fixed baseline comparison; normal operation requires TLS before redirection.
ReuseNative checks source and both executable hashes without rebuilding/rerunning.
SelfTest tests scalar decoding, export parsing and malformed-result rejection.
All generated files and compiler temporary files stay on the primary target
drive. Native helpers are hidden, bounded, and finally cleaned by exact ownership.
#>
param(
    [string]$OutputDirectory='',
    [string]$Runner='',
    [string[]]$Backends=@('interpreter','jit'),
    [string]$LlvmBin='C:\Program Files\LLVM\bin',
    [string]$WindowsSdk='C:\Program Files (x86)\Windows Kits\10',
    [string]$SdkVersion='10.0.26100.0',
    [switch]$NativeOnly,
    [switch]$ReuseNative,
    [switch]$ExpectBootstrapGap,
    [switch]$SelfTest
)
$ErrorActionPreference='Stop'
function Convert-StartupWords {
    param([byte[]]$Bytes,[int]$Count)
    if($Bytes.Length-ne $Count*4){throw 'startup scalar output has the wrong length'}
    return @(for($index=0;$index-lt $Count;$index++){[BitConverter]::ToUInt32($Bytes,$index*4)})
}
function Read-StartupWords {
    param([string]$Path,[int]$Count)
    Convert-StartupWords ([IO.File]::ReadAllBytes($Path)) $Count
}
function Assert-StartupWords {
    param([uint32[]]$Words,[uint32[]]$Expected,[int]$Offset,[string]$Label)
    for($index=0;$index-lt $Expected.Count;$index++){
        if($Words[$Offset+$index]-ne $Expected[$index]){throw "$Label mismatch at DWORD $($Offset+$index)"}
    }
}
function Get-StartupExport {
    param([string]$Text,[string]$Name)
    $pattern='(?s)Export\s*\{[^}]*?Name:\s*'+[regex]::Escape($Name)+'\s+RVA:\s*0x([0-9a-fA-F]+)\s*\}'
    $match=[regex]::Match($Text,$pattern)
    if(-not $match.Success){throw "owned child export is absent: $Name"}
    [Convert]::ToUInt32($match.Groups[1].Value,16)
}
function Assert-StartupChild {
    param([uint32[]]$Words,[bool]$Redirect,[bool]$Gap=$false)
    $path=if($Redirect){2}else{1}
    Assert-StartupWords $Words @(1229998403,2) 0 'child schema'
    Assert-StartupWords $Words @($path) 3 'child path'
    if($Gap){
        Assert-StartupWords $Words @(0) 2 'old bootstrap gap marker'
        Assert-StartupWords $Words @(0,127,1,2197,2943,16256,286335522,858997828,1431660134,2004322440,0,0,0,0,0,0) 4 'old bootstrap gap state'
    }else{
        Assert-StartupWords $Words @(324478056) 2 'TLS marker'
        Assert-StartupWords $Words @(1) 4 'TLS callback count'
        Assert-StartupWords $Words @(1919,24448) 8 'initialization FP control effects'
        Assert-StartupWords $Words @(1919,24448,2863315899,3435978205,4008640511,2576941056) 14 'callback FP disturbance'
        if($Redirect){
            Assert-StartupWords $Words @(127,1,2197) 5 'redirect GP/ESP/flags preservation'
            $requested=@(286335522,858997828,1431660134,2004322440)
            if(($Words[10..13]-join ',')-eq ($requested-join ',')){throw 'startup incorrectly restored requested XMM0 over callback effects'}
        }
    }
}
function Assert-StartupParent {
    param([uint32[]]$Words,[bool]$Gap=$false)
    Assert-StartupWords $Words @(1229998416,2) 0 'parent schema'
    Assert-StartupWords $Words @(1,1,0,4,1,0,0,1,1,0,1,37,1,1,0) 2 'normal child operations'
    $redirectExit=if($Gap){38}else{37}
    Assert-StartupWords $Words @(1,1,0,4,1,1,0,1,1,0,1,$redirectExit,1,1,0) 17 'redirect child operations'
    Assert-StartupWords $Words @(65579,65579,127,2197,2943,16256,286335522,858997828,1431660134,2004322440,1,2943,1,4) 32 'suspended requested state readback'
}
if($SelfTest){
    Assert-StartupWords (Convert-StartupWords ([byte[]]@(1,0,0,0,255,0,0,0)) 2) @(1,255) 0 'scalar decoder'
    $rejected=$false;try{Convert-StartupWords ([byte[]]@(1)) 1|Out-Null}catch{$rejected=$true}
    if(-not $rejected){throw 'decoder accepted a partial DWORD'}
    $exports="Export {`n  Name: redirect`n  RVA: 0x1020`n}`nExport {`n Name: expected_esp`n RVA: 0x3004`n}"
    if((Get-StartupExport $exports 'redirect')-ne 4128 -or (Get-StartupExport $exports 'expected_esp')-ne 12292){throw 'export parser failed'}
    $rejected=$false;try{Get-StartupExport $exports 'missing'|Out-Null}catch{$rejected=$true}
    if(-not $rejected){throw 'export parser accepted an absent symbol'}
    $child=[uint32[]]@(1229998403,2,324478056,2,1,127,1,2197,1919,24448,0,0,0,0,1919,24448,2863315899,3435978205,4008640511,2576941056)
    Assert-StartupChild $child $true
    $child[5]=126;$rejected=$false;try{Assert-StartupChild $child $true}catch{$rejected=$true}
    if(-not $rejected){throw 'contract check accepted a lost GP register'}
    $child[5]=127;$child[8]=2943;$rejected=$false;try{Assert-StartupChild $child $true}catch{$rejected=$true}
    if(-not $rejected){throw 'contract check accepted FP rollback'}
    $child[8]=1919;$requested=[uint32[]]@(286335522,858997828,1431660134,2004322440)
    for($index=0;$index-lt 4;$index++){$child[10+$index]=$requested[$index]}
    $rejected=$false;try{Assert-StartupChild $child $true}catch{$rejected=$true}
    if(-not $rejected){throw 'contract check accepted XMM rollback'}
    Write-Output 'initial-EIP wrapper self checks passed';exit 0
}
if(-not $OutputDirectory){throw 'OutputDirectory is required'}
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$common=(& git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir)
if($LASTEXITCODE-ne 0){throw 'startup fixture must run from its Git worktree'}
$primary=Split-Path -Parent ([IO.Path]::GetFullPath($common))
$allowed=[IO.Path]::GetFullPath((Join-Path $primary 'target/investigations'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){
    throw 'startup evidence must stay beneath primary target/investigations'
}
if([IO.Path]::GetPathRoot($OutputDirectory).TrimEnd('\')-eq $env:SystemDrive){throw 'startup artifacts cannot use the system drive'}
if($Backends|Where-Object{$_-notin @('interpreter','jit')}){throw 'unsupported startup backend'}
if(-not $NativeOnly -and -not $Runner){throw 'Runner is required for guest execution'}
$env:TEMP=Join-Path $OutputDirectory 'tmp';$env:TMP=$env:TEMP
if(-not $env:CARGO_TARGET_DIR){$env:CARGO_TARGET_DIR=Join-Path $primary 'target/fixture-build'}
if([IO.Path]::GetPathRoot([IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)).TrimEnd('\')-eq $env:SystemDrive){throw 'CARGO_TARGET_DIR cannot use the system drive'}
$native=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $native,$env:TEMP|Out-Null
$source=Join-Path $PSScriptRoot 'initial-eip-startup.c'
$parent=Join-Path $native 'initial-eip-parent.exe';$child=Join-Path $native 'initial-eip-child.exe'
$names=@('initial-eip-parent.bin','normal-child.bin','redirect-child.bin')
function Invoke-StartupProbe {
    param([string]$Executable,[string[]]$Arguments=@(),[string]$Directory,[string]$OwnedChild='',[int]$TimeoutMs=15000)
    $quoted=@($Arguments|ForEach-Object {'"'+([regex]::Replace([regex]::Replace($_,'(\\*)"','$1$1\"'),'(\\+)$','$1$1'))+'"'})
    if($OwnedChild -and (Get-CimInstance Win32_Process|Where-Object{$_.ExecutablePath-eq $OwnedChild})){throw 'owned child is already running'}
    $options=@{FilePath=$Executable;WorkingDirectory=$Directory;WindowStyle='Hidden';PassThru=$true;RedirectStandardOutput=(Join-Path $Directory 'run.log');RedirectStandardError=(Join-Path $Directory 'stderr.log')}
    if($quoted.Count){$options.ArgumentList=$quoted}
    $process=Start-Process @options
    $ownerId=$process.Id
    try{
        if(-not $process.WaitForExit($TimeoutMs)){throw 'startup helper exceeded its bounded runtime'}
        $process.Refresh();return $process.ExitCode
    }finally{
        try{
            if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null}
            if($OwnedChild){
                foreach($owned in (Get-CimInstance Win32_Process|Where-Object{$_.ExecutablePath-eq $OwnedChild -and $_.ParentProcessId-eq $ownerId})){
                    Stop-Process -Id $owned.ProcessId -Force -ErrorAction SilentlyContinue
                }
            }
        }finally{$process.Dispose()}
    }
}
$environment=@("TEMP=$env:TEMP","TMP=$env:TMP","CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR")
$provenance=Join-Path $native 'cmd.txt'
if($ReuseNative){
    $saved=Get-Content -LiteralPath $provenance -Raw
    foreach($item in @(@($source,'source'),@($parent,'parent'),@($child,'child'))){
        if(-not $saved.Contains("$($item[1])_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath $item[0]).Hash)")){throw 'saved startup source/executable hash differs'}
    }
}else{
    $compile=@('/nologo','/c','/Od','/GS-','/Gs9999999','/Zl','/clang:-fno-builtin','/clang:--target=i686-pc-windows-msvc',"/I$native","/I$WindowsSdk/Include/$SdkVersion/um","/I$WindowsSdk/Include/$SdkVersion/shared")
    $link=@('/nologo','/machine:x86','/nodefaultlib','/dynamicbase:no','/base:0x400000','/fixed:no','/opt:noref','/opt:noicf','/safeseh:no','/timestamp:0',"/libpath:$WindowsSdk/Lib/$SdkVersion/um/x86",'/subsystem:console','/entry:mainCRTStartup')
    & (Join-Path $LlvmBin 'clang-cl.exe') @compile /DCHILD=1 $source "/Fo$native/child.obj"
    if($LASTEXITCODE-ne 0){throw 'startup child compilation failed'}
    & (Join-Path $LlvmBin 'lld-link.exe') @link "/out:$child" "$native/child.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw 'startup child link failed'}
    $exports=(& (Join-Path $LlvmBin 'llvm-readobj.exe') --coff-exports $child)-join "`n"
    if($LASTEXITCODE-ne 0){throw 'startup export inspection failed'}
    @('/* Generated only from the owned child export directory. */','#define CHILD_BASE 0x400000u',"#define CHILD_REDIRECT_RVA $(Get-StartupExport $exports 'redirect')u","#define CHILD_MARKER_RVA $(Get-StartupExport $exports 'tls_marker')u","#define CHILD_ESP_RVA $(Get-StartupExport $exports 'expected_esp')u")|Set-Content -LiteralPath (Join-Path $native 'initial-eip-layout.h') -Encoding ASCII
    & (Join-Path $LlvmBin 'clang-cl.exe') @compile $source "/Fo$native/parent.obj"
    if($LASTEXITCODE-ne 0){throw 'startup parent compilation failed'}
    & (Join-Path $LlvmBin 'lld-link.exe') @link "/out:$parent" "$native/parent.obj" kernel32.lib
    if($LASTEXITCODE-ne 0){throw 'startup parent link failed'}
    @("compile=clang-cl $($compile-join ' ') source (/DCHILD=1 for child)","link=lld-link $($link-join ' ') role.obj kernel32.lib","command=$parent","source_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash)","parent_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath $parent).Hash)","child_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath $child).Hash)")+$environment|Set-Content -LiteralPath $provenance
    foreach($name in $names){$old=Join-Path $native $name;if(Test-Path -LiteralPath $old){Remove-Item -LiteralPath $old}}
    if((Invoke-StartupProbe -Executable $parent -Directory $native -OwnedChild $child)-ne 0){throw 'native startup helper failed'}
}
$reference=@(Read-StartupWords (Join-Path $native $names[0]) 46)
$normal=@(Read-StartupWords (Join-Path $native $names[1]) 20)
$redirect=@(Read-StartupWords (Join-Path $native $names[2]) 20)
Assert-StartupParent $reference;Assert-StartupChild $normal $false;Assert-StartupChild $redirect $true
$result=[ordered]@{schema=2;native=[ordered]@{parent=$reference;normal=$normal;redirect=$redirect};guest=[ordered]@{};xmm0_policy='Compare callback disturbance and absence of rollback; native loader XMM0 contents are observations.'}
$result|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $OutputDirectory 'comparison.json')
Write-Output 'native: TLS precedes initial EIP; GP/ESP/status flags preserved; initialization FP effects remain'
if($NativeOnly){exit 0}
$Runner=[IO.Path]::GetFullPath($Runner)
$bin=Join-Path $OutputDirectory 'bin';New-Item -ItemType Directory -Force -Path $bin|Out-Null
$runRunner=Join-Path $bin 'adv-run.exe'
if(-not $Runner.Equals([IO.Path]::GetFullPath($runRunner),[StringComparison]::OrdinalIgnoreCase)){Copy-Item -LiteralPath $Runner -Destination $runRunner -Force}
foreach($fixture in @($parent,$child)){Copy-Item -LiteralPath $fixture -Destination $bin -Force}
foreach($pattern in @('avcodec-*.dll','avformat-*.dll','avutil-*.dll','swresample-*.dll','swscale-*.dll')){
    foreach($dll in (Get-ChildItem -LiteralPath (Split-Path -Parent $Runner) -Filter $pattern -File)){
        $destination=Join-Path $bin $dll.Name
        if(Test-Path -LiteralPath $destination){if((Get-FileHash $destination).Hash-ne (Get-FileHash $dll.FullName).Hash){throw 'frozen media dependency hash differs'}}
        else{New-Item -ItemType HardLink -Path $destination -Target $dll.FullName|Out-Null}
    }
}
foreach($backend in $Backends){
    $directory=Join-Path $OutputDirectory $backend;New-Item -ItemType Directory -Force -Path $directory|Out-Null
    $data=Join-Path $directory 'data';$report=Join-Path $directory 'report.json'
    foreach($name in $names){$old=Join-Path $data "overlay/$name";if(Test-Path -LiteralPath $old){Remove-Item -LiteralPath $old}}
    if(Test-Path -LiteralPath $report){Remove-Item -LiteralPath $report}
    $arguments=@((Join-Path $bin 'initial-eip-parent.exe'),'--headless','--backend',$backend,'--seconds','10','--data-dir',$data,'--report',$report,'--trace',(Join-Path $directory 'api.log'),'--api-mode','strict')
    if($backend-eq 'jit'){$arguments+='--sync-compilation'}
    @("command=$runRunner $($arguments-join ' ')","runner_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath $runRunner).Hash)","parent_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $bin 'initial-eip-parent.exe')).Hash)","child_sha256=$((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $bin 'initial-eip-child.exe')).Hash)")+$environment|Set-Content -LiteralPath (Join-Path $directory 'cmd.txt')
    $null=Invoke-StartupProbe -Executable $runRunner -Arguments $arguments -Directory $directory
    $actual=@(Read-StartupWords (Join-Path $data "overlay/$($names[0])") 46)
    $actualNormal=@(Read-StartupWords (Join-Path $data "overlay/$($names[1])") 20)
    $actualRedirect=@(Read-StartupWords (Join-Path $data "overlay/$($names[2])") 20)
    $e=Get-Content -LiteralPath $report -Raw|ConvertFrom-Json
    if($e.backend-ne $backend -or $e.stop_reason-ne 'guest_exit' -or $e.failure -or $e.api.acceptance_eligible-ne $true -or $e.api.stub_calls-ne 0 -or $e.api.warnings.Count-ne 0 -or @($e.api.called_gaps.PSObject.Properties).Count-ne 0){throw "$backend strict startup report failed"}
    if($backend-eq 'jit' -and $e.native_entries-le 0){throw 'startup JIT fixture did not execute translated code'}
    $result.guest[$backend]=[ordered]@{parent=$actual;normal=$actualNormal;redirect=$actualRedirect;expected_bootstrap_gap=[bool]$ExpectBootstrapGap}
    $result|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $OutputDirectory 'comparison.json')
    Assert-StartupParent $actual ([bool]$ExpectBootstrapGap)
    Assert-StartupChild $actualNormal $false
    Assert-StartupChild $actualRedirect $true ([bool]$ExpectBootstrapGap)
    Write-Output "${backend}: startup scalar contracts pass (expected old gap=$ExpectBootstrapGap)"
}
