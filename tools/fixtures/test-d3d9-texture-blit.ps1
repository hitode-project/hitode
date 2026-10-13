<#
.SYNOPSIS
Compare authored RGBX conversion, texture targets and render-state roundtrips with native D3DX9.
.DESCRIPTION
Inputs: GPU runner and installed native D3DX9. Builds an authored x86 fixture,
freezes the runner in the evidence directory and compares interpreter/JIT scalar
outputs. Writes isolated overlays, reports, cmd.txt and two executable hashes;
console output contains assertion counts only. No game sample is required.
#>
param(
 [string]$Runner=(Join-Path $PSScriptRoot '../../target/release/adv-run.exe'),
 [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/d3d9-texture-blit')
)
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target'))
if(-not $OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'evidence must stay inside workspace target'}
$native=Join-Path $OutputDirectory 'native'
& (Join-Path $PSScriptRoot 'build-d3d9.ps1') -OutputDirectory $native -Name d3d9-texture-blit
$fixture=Join-Path $native 'fixture_d3d9-texture-blit.exe'
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
function Read-Words([string]$Path){
 $b=[IO.File]::ReadAllBytes($Path)
 if($b.Length-lt 200 -or $b.Length%4-ne 0){throw 'incomplete assertions'}
 $a=@();for($i=0;$i-lt $b.Length;$i+=4){$a += [BitConverter]::ToUInt32($b,$i)}
 return $a
}
Run-Owned $fixture @() $native
$reference=Read-Words (Join-Path $native 'd3d9-texture-blit.bin')
if($reference.Count-ne 816){throw 'native observation count changed'}
foreach($part in @(@(23,16,12,0xff765a1fL,0x33112244),@(288,8,8,0xff445522L,0x221122ff),@(554,8,8,0xff223355L,0x11223344))){
 for($y=0;$y-lt 16;$y++){for($x=0;$x-lt 16;$x++){
  $inside=if($part[0]-eq 23){$y-lt 12}else{$x-ge 4 -and $x-lt 12 -and $y-ge 4 -and $y-lt 12}
  $want=if($inside){[uint32]$part[3]}else{[uint32]$part[4]}
  if($reference[$part[0]+$y*16+$x]-ne $want){throw 'native defined texture pixel differs'}
 }}
}
Write-Output "native: $($reference.Count) scalar observations, 768 independently checked RGBA pixels"
foreach($backend in @('interpreter','jit')){
 $folder=Join-Path $OutputDirectory $backend
 New-Item -ItemType Directory -Force $folder | Out-Null
 Run-Owned $runnerCopy @($fixture,'--headless','--backend',$backend,'--headless-gpu','--seconds','15','--data-dir',(Join-Path $folder 'data'),'--report',(Join-Path $folder 'report.json')) $folder
 $actual=Read-Words (Join-Path $folder 'data/overlay/d3d9-texture-blit.bin')
 if($actual.Count-ne $reference.Count){throw 'scalar count mismatch'}
 for($i=0;$i-lt $reference.Count;$i++){if($actual[$i]-ne $reference[$i]){throw "$backend scalar mismatch at $i"}}
 $report=Get-Content -Raw -LiteralPath (Join-Path $folder 'report.json') | ConvertFrom-Json
 if($report.guest_exit_code-ne 0 -or -not $report.api.acceptance_eligible){throw 'strict guest acceptance failed'}
 Write-Output "$backend`: $($actual.Count) scalars match native"
}
