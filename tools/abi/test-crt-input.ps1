<#
.SYNOPSIS
Verify CRT regeneration and rejection of changed x86 process/math contracts.
.DESCRIPTION
Run pwsh -NoProfile -File tools/abi/test-crt-input.ps1 after generate.ps1 -Download.
Input: pinned WinMD and repository CRT declarations. Output: generated fixtures
under target/tmp/abi-metadata/crt-tests and a pass/fail summary.
#>
param([string]$Winmd = (Join-Path $PSScriptRoot '../../target/tmp/abi-metadata/package/Windows.Win32.winmd'))
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$source = Get-Content (Join-Path $root 'data/abi/source.json') -Raw | ConvertFrom-Json
if ((Get-FileHash $Winmd -Algorithm SHA256).Hash -ne $source.winmd_sha256) { throw 'Pinned WinMD hash mismatch' }
Add-Type -Path (Join-Path $PSScriptRoot 'Metadata.cs')
$cache = Join-Path $root 'target/tmp/abi-metadata/crt-tests'
New-Item -ItemType Directory -Force $cache | Out-Null
$modules = Join-Path $root 'data/abi/modules.txt'
$selected = Join-Path $root 'data/abi/selected-exports.tsv'
$sdk = Join-Path $root 'data/abi/sdk-input.tsv'
$crt = Join-Path $root 'data/abi/crt-input.tsv'
[AbiMetadata]::Generate($Winmd, (Join-Path $cache 'valid'), $modules, $crt, $selected, $sdk)
foreach ($name in @('signatures.tsv', 'structs.rs', 'com.tsv', 'constants.tsv')) {
    $expected = (Get-FileHash (Join-Path $root "data/abi/$name") -Algorithm SHA256).Hash
    $actual = (Get-FileHash (Join-Path $cache "valid/$name") -Algorithm SHA256).Hash
    if ($expected -ne $actual) { throw "CRT regeneration differs: $name" }
}
$rows = [IO.File]::ReadAllLines($crt)
function Reject-Change([string]$Name, [string]$Export, [int]$Field, [string]$Value, [string]$Reason) {
    $changed = [string[]]$rows.Clone()
    $found = $false
    for ($i = 0; $i -lt $changed.Length; $i++) {
        if ($changed[$i].StartsWith($Export + "`t")) {
            $fields = $changed[$i].Split("`t")
            $fields[$Field] = $Value
            $changed[$i] = $fields -join "`t"
            $found = $true
            break
        }
    }
    if (!$found) { throw "Test input missing: $Name" }
    $inputPath = Join-Path $cache "$Name.tsv"
    [IO.File]::WriteAllLines($inputPath, $changed)
    try { [AbiMetadata]::Generate($Winmd, (Join-Path $cache $Name), $modules, $inputPath, $selected, $sdk) }
    catch { if ($_.Exception.ToString().Contains($Reason)) { return }; throw }
    throw "Changed CRT declaration accepted: $Name"
}
Reject-Change 'list-convention' "ucrtbase.dll`t_execl" 2 'cdecl' 'process list stack contract'
Reject-Change 'list-result' "ucrtbase.dll`t_execl" 3 'ptr' 'process list stack contract'
Reject-Change 'spawn-prefix' "ucrtbase.dll`t_spawnl" 5 'path:const char*:str8:1|arg0:const char*:str8:1' 'process list stack contract'
Reject-Change 'wide-list' "ucrtbase.dll`t_wexecl" 5 'path:const char*:str8:1|arg0:const char*:str8:1' 'process list stack contract'
Reject-Change 'pow-result' "msvcrt.dll`tpow" 3 'i32' 'binary math stack contract'
Reject-Change 'hypot-width' "msvcrt.dll`t_hypot" 5 'x:double:f64:1|y:double:f64:2' 'binary math stack contract'
Reject-Change 'throw-convention' "msvcr90.dll`t_CxxThrowException" 2 'cdecl' 'C++ throw stack contract'
Reject-Change 'throw-width' "msvcr90.dll`t_CxxThrowException" 5 'object:void*:ptr:1' 'C++ throw stack contract'
Reject-Change 'setjmp-convention' "msvcr90.dll`t_setjmp3" 2 'cdecl' 'setjmp stack contract'
Reject-Change 'setjmp-prefix' "msvcr90.dll`t_setjmp3" 5 'buffer:jmp_buf:ptr:1' 'setjmp stack contract'
Reject-Change 'cpp-jump-callback' "msvcr90.dll`t__CxxLongjmpUnwind" 2 'cdecl' 'C++ local unwind callback stack contract'
Reject-Change 'float-data-convention' "msvcr90.dll`t_HUGE" 2 'cdecl' 'Invalid supplemental x86 signature'
Reject-Change 'float-data-width' "msvcr90.dll`t_HUGE" 3 'f32' 'Invalid supplemental x86 signature'
Reject-Change 'integer-wide-result' "msvcr90.dll`t_strtoi64" 3 'i32' 'integer conversion stack contract'
Reject-Change 'integer-unsigned-result' "ucrtbase.dll`tstrtoull" 3 'i64' 'integer conversion stack contract'
Reject-Change 'integer-wide-input' "ucrtbase.dll`twcstol" 5 'text:const char*:str8:1|end:char**:ptr:1|base:int:i32:1' 'integer conversion stack contract'
Reject-Change 'integer-prefix-arguments' "ucrtbase.dll`tatoi" 5 'text:const char*:str8:1|base:int:i32:1' 'integer conversion stack contract'
Reject-Change 'integer-format-value-width' "msvcr90.dll`t_ui64toa_s" 5 'value:unsigned __int64:u64:1|buffer:char*:ptr:1|capacity:size_t:u32:1|radix:int:i32:1' 'integer formatting stack contract'
Reject-Change 'integer-format-output-width' "msvcr90.dll`t_ui64tow_s" 5 'value:unsigned __int64:u64:2|buffer:char*:ptr:1|capacity:size_t:u32:1|radix:int:i32:1' 'integer formatting stack contract'
Reject-Change 'integer-format-result' "ucrtbase.dll`t_itoa" 3 'i32' 'integer formatting stack contract'
Reject-Change 'integer-format-capacity' "ucrtbase.dll`t_itoa_s" 5 'value:int:i32:1|buffer:char*:ptr:1|radix:int:i32:1' 'integer formatting stack contract'
Write-Output 'CRT inputs: reproducible generation and twenty-one malformed process/math/throw/jump/data/integer contracts verified.'
