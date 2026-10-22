param([string]$Winmd = (Join-Path $PSScriptRoot '../../target/tmp/abi-metadata/package/Windows.Win32.winmd'))
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$cache = Join-Path $repoRoot 'target/tmp/abi-metadata/selected-tests'
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$source = Get-Content -LiteralPath (Join-Path $repoRoot 'data/abi/source.json') -Raw | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $Winmd -Algorithm SHA256).Hash -ne $source.winmd_sha256) { throw 'Pinned WinMD hash mismatch' }
Add-Type -Path (Join-Path $PSScriptRoot 'Metadata.cs')
$modules = Join-Path $repoRoot 'data/abi/modules.txt'
$crt = Join-Path $repoRoot 'data/abi/crt-input.tsv'
$selections = Get-Content -LiteralPath (Join-Path $repoRoot 'data/abi/selected-exports.tsv') | Where-Object { $_ -and -not $_.StartsWith('#') }
$valid = Join-Path $cache 'valid.tsv'
[IO.File]::WriteAllLines($valid, $selections)
$validOutput = Join-Path $cache 'valid'
[AbiMetadata]::Generate($Winmd, $validOutput, $modules, $crt, $valid)
$rows = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $validOutput 'signatures.tsv')) {
    if ($line.StartsWith('#')) { continue }
    $fields = $line.Split("`t")
    $rows[$fields[0] + '!' + $fields[1]] = $fields
}
$expectedScope = @{ 'ntdll.dll' = 3; 'shfolder.dll' = 2; 'imagehlp.dll' = 1 }
foreach ($module in $expectedScope.Keys) {
    if (($rows.Keys | Where-Object { $_.StartsWith($module + '!') }).Count -ne $expectedScope[$module]) { throw "Selection widened $module" }
}
foreach ($suffix in @('A', 'W')) {
    $alias = $rows['shfolder.dll!SHGetFolderPath' + $suffix]
    $original = $rows['shell32.dll!SHGetFolderPath' + $suffix]
    if (($alias[2..5] -join "`t") -cne ($original[2..5] -join "`t")) { throw 'Shell alias changed its source declaration' }
}
if (($rows['kernel32.dll!lstrcpy'][2..5] -join "`t") -cne ($rows['kernel32.dll!lstrcpyA'][2..5] -join "`t")) { throw 'Legacy ANSI alias changed its source declaration' }
if (($rows['imagehlp.dll!MakeSureDirectoryPathExists'][2..5] -join "`t") -cne ($rows['dbghelp.dll!MakeSureDirectoryPathExists'][2..5] -join "`t")) { throw 'ImageHlp alias changed its source declaration' }
# Every selected alias retains the entire source ABI, including expanded DLLs.
foreach ($selection in $selections) {
    $fields = $selection.Split("`t")
    $alias = $rows[$fields[0] + '!' + $fields[1]]
    $original = $rows[$fields[2] + '!' + $fields[3]]
    if (($alias[2..5] -join "`t") -cne ($original[2..5] -join "`t")) { throw 'Selected alias changed its source declaration' }
}
# Existing binary64 CRT input must regenerate, but malformed stack widths and
# unrelated binary64 declarations must not be admitted by that support.
$crtLines = [IO.File]::ReadAllLines($crt)
foreach ($name in @('pow', '_hypot')) {
    $badCrt = $crtLines.Clone()
    $index = [Array]::FindIndex($badCrt, [Predicate[string]] { param($line) $line.StartsWith("msvcrt.dll`t$name`t") })
    if ($index -lt 0) { throw "Missing binary64 CRT input: $name" }
    $badCrt[$index] = $badCrt[$index].Replace('x:double:f64:2', 'x:double:f64:1')
    $badPath = Join-Path $cache ($name + '-bad-crt.tsv')
    [IO.File]::WriteAllLines($badPath, $badCrt)
    $rejected = $false
    try { [AbiMetadata]::Generate($Winmd, (Join-Path $cache ($name + '-bad-crt')), $modules, $badPath, $valid) }
    catch { if (!$_.Exception.ToString().Contains('Invalid supplemental x86 signature')) { throw }; $rejected = $true }
    if (!$rejected) { throw "Malformed binary64 CRT input accepted: $name" }
}
function Assert-Rejected([string]$Name, [string[]]$InputLines, [string]$Reason) {
    $inputPath = Join-Path $cache ($Name + '.tsv')
    [IO.File]::WriteAllLines($inputPath, $InputLines)
    try { [AbiMetadata]::Generate($Winmd, (Join-Path $cache $Name), $modules, $crt, $inputPath) }
    catch { if ($_.Exception.ToString().Contains($Reason)) { return }; throw }
    throw "Selected exports unexpectedly accepted $Name"
}
Assert-Rejected 'missing' ($selections + "ntdll.dll`tRtlFormatCurrentUserKeyPath`tntdll.dll`tRtlFormatCurrentUserKeyPath`t1") 'missing from pinned WinMD'
$wrongWords = $selections.Clone()
$wrongWords[0] = $wrongWords[0].Substring(0, $wrongWords[0].LastIndexOf("`t") + 1) + '5'
Assert-Rejected 'wrong-words' $wrongWords 'stack contract changed'
Assert-Rejected 'duplicate' ($selections + $selections[0]) 'Duplicate selected'
Assert-Rejected 'conflict' ($selections + "user32.dll`tGetWindowPlacement`tshell32.dll`tSHGetFolderPathA`t5") 'Conflicting selected'
Write-Output 'Selected exports: exact DLL scope and complete aliases verified; missing source, changed stack width, duplicate destination and conflicting alias rejected.'
