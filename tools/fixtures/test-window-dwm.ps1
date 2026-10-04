param(
    [string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/sample-loop/dwm-owned-final'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
$env:ADV_AUDIO_MUTE_DEVICE='1'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'fixture evidence must stay inside workspace target'}
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
function Read-Records([string]$Path,[uint32]$Marker,[int]$ExpectedCount) {
    $bytes=[IO.File]::ReadAllBytes($Path)
    if($bytes.Length%8-ne 0){throw 'partial fixture evidence'}
    $records=@{}
    for($at=0;$at-lt $bytes.Length;$at+=8){
        $tag=[BitConverter]::ToUInt32($bytes,$at)
        if($records.ContainsKey($tag)){throw 'duplicate fixture tag'}
        $records[$tag]=[BitConverter]::ToUInt32($bytes,$at+4)
    }
    if($records.Count-ne $ExpectedCount -or $records[[uint32]9999]-ne $Marker){throw 'incomplete fixture marker/count'}
    return $records
}
function Run-Owned([string]$Exe,[string]$Directory,[string[]]$Arguments,[string]$LogPrefix) {
    $options=@{FilePath=$Exe;WorkingDirectory=$Directory;WindowStyle='Hidden';PassThru=$true}
    if($Arguments.Count){$options.ArgumentList=($Arguments|ForEach-Object{'"'+$_+'"'})-join ' '}
    if($LogPrefix){$options.RedirectStandardOutput="$LogPrefix.stdout.log";$options.RedirectStandardError="$LogPrefix.stderr.log"}
    $process=Start-Process @options
    try {if(-not $process.WaitForExit(5000)){throw 'owned fixture timed out'};if($process.ExitCode-ne 0){throw "owned fixture exit $($process.ExitCode)"}}
    finally {if(-not $process.HasExited){$process.Kill();$process.WaitForExit(1000)|Out-Null};$process.Dispose()}
}
foreach($fixture in @(@{Name='window-dwm-scope';Count=104;Marker=[uint32]0x44574d53},@{Name='window-dwm-wait';Count=120;Marker=[uint32]0x44574d57})) {
    $name=$fixture.Name
    $native=Join-Path $OutputDirectory "$name/native"
    New-Item -ItemType Directory -Force -Path $native|Out-Null
    & "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot "$name.c") "/Fo$native/$name.obj"
    if($LASTEXITCODE-ne 0){throw 'fixture compile failed'}
    $exe=Join-Path $native "fixture_$name.exe"
    & "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 /subsystem:windows /entry:mainCRTStartup "/libpath:$sdk" "/out:$exe" "$native/$name.obj" kernel32.lib user32.lib
    if($LASTEXITCODE-ne 0){throw 'fixture link failed'}
    $nativeOutput=Join-Path $native "$name.bin"
    if(Test-Path -LiteralPath $nativeOutput){Remove-Item -LiteralPath $nativeOutput}
    Run-Owned $exe $native @() ''
    $reference=Read-Records $nativeOutput $fixture.Marker $fixture.Count
    Write-Output "$name native: $($reference.Count) tagged results."
    if($NativeOnly){continue}
    foreach($backend in @('interpreter','jit')) {
        $folder=Join-Path $OutputDirectory "$name/$backend"
        New-Item -ItemType Directory -Force -Path $folder|Out-Null
        $output=Join-Path $folder "data/overlay/$name.bin"
        if(Test-Path -LiteralPath $output){Remove-Item -LiteralPath $output}
        $arguments=@($exe,'--headless','--backend',$backend,'--seconds','3','--data-dir',(Join-Path $folder 'data'),'--report',(Join-Path $folder 'report.json'))
        Run-Owned ([IO.Path]::GetFullPath($Runner)) $folder $arguments (Join-Path $folder 'guest')
        $actual=Read-Records $output $fixture.Marker $fixture.Count
        foreach($tag in $reference.Keys) {
            if(-not $actual.ContainsKey($tag)-or $reference[$tag]-ne $actual[$tag]){throw ('{0} {1} tag {2}: native {3:X8}, guest {4:X8}'-f $name,$backend,$tag,$reference[$tag],$actual[$tag])}
        }
        $report=Get-Content -LiteralPath (Join-Path $folder 'report.json') -Raw|ConvertFrom-Json
        if($report.guest_exit_code-ne 0 -or $report.api.acceptance_eligible-ne $true){throw 'guest strict acceptance failed'}
        Write-Output "$name $backend`: $($reference.Count) exact tagged results."
    }
}
