<#
.SYNOPSIS
Compare authored BMP export and ID3DXBuffer lifetime assertions with native D3DX9.
.DESCRIPTION
Inputs: GPU runner and installed native D3DX9. Builds an authored x86 fixture,
freezes the runner in the evidence directory and compares interpreter/JIT boolean
outputs. Writes isolated overlays, reports, cmd.txt and two executable hashes;
console output contains assertion counts only. No game sample is required.
#>
param(
 [string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),
 [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/d3dx9-surface-save')
)
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'evidence must stay inside workspace target'}
$native=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $native -Name d3dx9-surface-save
$fixture=Join-Path $native 'fixture_d3dx9-surface-save.exe'
$bin=Join-Path $OutputDirectory 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item -LiteralPath $Runner -Destination (Join-Path $bin 'adv-run.exe')
Get-ChildItem -LiteralPath (Split-Path $Runner) -Filter '*.dll' | Copy-Item -Destination $bin
$runnerCopy=Join-Path $bin 'adv-run.exe'
function Run-Owned([string]$Exe,[string[]]$Arguments,[string]$Folder){
 $line=($Arguments | ForEach-Object {'"'+$_+'"'}) -join ' '
 @("Command: $Exe $line",'Environment: inherited; no sample input',"Runner SHA-256: $((Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash)","Guest SHA-256: $((Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash)") | Set-Content -LiteralPath (Join-Path $Folder 'cmd.txt')
 $options=@{FilePath=$Exe;WorkingDirectory=$Folder;WindowStyle='Hidden';PassThru=$true;RedirectStandardOutput=(Join-Path $Folder 'stdout.log');RedirectStandardError=(Join-Path $Folder 'stderr.log')}
 if($line){$options.ArgumentList=$line}
 $p=Start-Process @options
 try { if(-not $p.WaitForExit(30000)){throw 'authored fixture timed out'};if($p.ExitCode-ne 0){throw "authored fixture exit $($p.ExitCode)"} }
 finally {if(-not $p.HasExited){$p.Kill();$p.WaitForExit()};$p.Dispose()}
}
function Read-Checks([string]$Path){
 $b=[IO.File]::ReadAllBytes($Path)
 if($b.Length-lt 200 -or $b.Length%4-ne 0){throw 'incomplete assertions'}
 $a=@();for($i=0;$i-lt $b.Length;$i+=4){$a += [BitConverter]::ToUInt32($b,$i)}
 if(@($a | Where-Object {$_-ne 1}).Count-ne 0){throw 'an authored invariant failed'}
 return $a
}
Run-Owned $fixture @() $native
$reference=Read-Checks (Join-Path $native 'd3dx9-surface-save.bin')
Write-Output "native: $($reference.Count) assertions passed"
foreach($backend in @('interpreter','jit')){
 $folder=Join-Path $OutputDirectory $backend
 New-Item -ItemType Directory -Force $folder | Out-Null
 Run-Owned $runnerCopy @($fixture,'--headless','--backend',$backend,'--seconds','10','--data-dir',(Join-Path $folder 'data'),'--report',(Join-Path $folder 'report.json')) $folder
 $actual=Read-Checks (Join-Path $folder 'data/overlay/d3dx9-surface-save.bin')
 if($actual.Count-ne $reference.Count){throw 'assertion count mismatch'}
 $report=Get-Content -Raw -LiteralPath (Join-Path $folder 'report.json') | ConvertFrom-Json
 if($report.guest_exit_code-ne 0 -or -not $report.api.acceptance_eligible){throw 'strict guest acceptance failed'}
 Write-Output "$backend`: $($actual.Count) assertions match native"
}
