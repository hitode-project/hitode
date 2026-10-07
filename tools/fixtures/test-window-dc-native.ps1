param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/investigations/window-dc/native'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
& (Join-Path $PSScriptRoot 'build-window-dc-native.ps1') -OutputDirectory $OutputDirectory
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'window-dc-native.c') -Destination (Join-Path $OutputDirectory 'fixture-source.c')
$binary = Join-Path $OutputDirectory 'window-dc-native.exe'
$binaryHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash
$sourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $OutputDirectory 'fixture-source.c')).Hash
@(
    ("Command: & '" + (Join-Path $PSScriptRoot 'test-window-dc-native.ps1').Replace("'", "''") + "' -OutputDirectory '" + $OutputDirectory.Replace("'", "''") + "'")
    'Environment: native Windows; x86 authored fixture; no task-specific environment variables.'
    ('OS: ' + [Runtime.InteropServices.RuntimeInformation]::OSDescription)
    ('Fixture SHA-256: ' + $binaryHash)
    ('Source SHA-256: ' + $sourceHash)
    'No game or adv-run involved; game SHA-256 and --data-dir are not applicable.'
) | Set-Content -LiteralPath (Join-Path $OutputDirectory 'cmd.txt')
$process = Start-Process -FilePath $binary -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
if (-not $process.WaitForExit(30000)) {
    $process.Kill()
    throw 'native window DC fixture timed out'
}
if ($process.ExitCode -ne 0) { throw "native window DC fixture exited $($process.ExitCode)" }
$bytes = [IO.File]::ReadAllBytes((Join-Path $OutputDirectory 'window-dc-native.bin'))
$words = for ($index = 0; $index -lt $bytes.Length; $index += 4) { [BitConverter]::ToUInt32($bytes, $index) }
if ($words.Count -ne 220 -or $words[0] -ne 0x44434c31 -or $words[141] -ne 0x44434c32) { throw 'unexpected native report layout' }
function Read-State([int]$Offset) {
    [ordered]@{
        stretch = $words[$Offset]; background_mode = $words[$Offset + 1]
        background_color = $words[$Offset + 2]; text_color = $words[$Offset + 3]
        system_font = $words[$Offset + 4]; fixed_font = $words[$Offset + 5]; variable_font = $words[$Offset + 6]
    }
}
function Require($Value, $Expected, [string]$Description) {
    if ($Value -ne $Expected) { throw "$Description expected $Expected, got $Value" }
}
function Require-State($Value, $Expected, [string]$Description) {
    foreach ($key in $Expected.Keys) { Require $Value[$key] $Expected[$key] "$Description/$key" }
}
$cases = foreach ($case in 0..1) {
    $offset = 1 + 70 * $case
    foreach ($field in @(1, 2, 3, 15, 16, 17, 18, 36, 37, 38, 47, 48, 57, 60, 69)) {
        Require $words[$offset + $field] 1 "case $case setup/lifecycle field $field"
    }
    [ordered]@{
        class_style = $words[$offset]; initial = (Read-State ($offset + 4))
        release = $words[$offset + 17]; same_on_reacquire = $words[$offset + 19]
        reacquired = (Read-State ($offset + 20)); restore = $words[$offset + 27]
        restore_error = $words[$offset + 28]; restored = (Read-State ($offset + 29))
        paint_same_dc = $words[$offset + 39]; paint = (Read-State ($offset + 40))
        after_paint = (Read-State ($offset + 50))
        type_after_release = $words[$offset + 58]; type_after_destroy = $words[$offset + 61]
        mode_after_destroy = $words[$offset + 63]; mode_error_after_destroy = $words[$offset + 64]
        release_after_destroy = $words[$offset + 65]; release_error_after_destroy = $words[$offset + 66]
        get_dc_after_destroy = $words[$offset + 67]; get_dc_error_after_destroy = $words[$offset + 68]
    }
}
Require-State $cases[0].reacquired $cases[0].initial 'common reacquisition resets attributes'
Require-State $cases[0].paint $cases[0].initial 'common BeginPaint resets attributes'
Require $cases[0].restore 0 'common acquisition discards SaveDC stack'
Require $cases[1].same_on_reacquire 1 'private DC identity'
Require $cases[1].reacquired.stretch 4 'private DC retains changed stretch'
Require $cases[1].reacquired.variable_font 1 'private DC retains changed font'
Require $cases[1].reacquired.background_color 0x060504 'private DC retains changed background'
Require $cases[1].reacquired.text_color 0x030201 'private DC retains changed text color'
Require $cases[1].restore 1 'private DC retains SaveDC stack'
Require $cases[1].restored.stretch 3 'private RestoreDC restores stretch'
Require $cases[1].restored.fixed_font 1 'private RestoreDC restores font'
Require $cases[1].restored.background_color 0x332211 'private RestoreDC restores background'
Require $cases[1].restored.text_color 0x665544 'private RestoreDC restores text color'
Require-State $cases[1].paint $cases[1].restored 'private BeginPaint retains restored attributes'
Require-State $cases[1].after_paint $cases[1].restored 'private EndPaint retains restored attributes'
foreach ($case in $cases) {
    Require $case.mode_after_destroy 0 'retired DC mode query'
    Require $case.release_after_destroy 0 'retired DC release'
    Require $case.get_dc_after_destroy 0 'destroyed window GetDC'
    Require $case.get_dc_error_after_destroy 1400 'destroyed window GetDC error'
}
$extra = [ordered]@{
    distinct_private_dcs = $words[147]; first = (Read-State 152); second = (Read-State 159)
    window_dc_matches_private = $words[167]; window_dc = (Read-State 168)
    repeated_window_dc = (Read-State 179); private_after_window_dc = (Read-State 187)
    second_after_window_dc = (Read-State 194); destroyed_first_mode = $words[202]
    destroyed_first_release = $words[204]; destroyed_first_release_error = $words[205]
    second_after_first_destroy = (Read-State 208); destroyed_second_mode = $words[217]
}
foreach ($field in @(142, 143, 144, 145, 146, 148, 149, 150, 151, 166, 176, 177, 186, 201, 215, 216, 219)) {
    Require $words[$field] 1 "independent-window setup/lifecycle field $field"
}
Require $extra.distinct_private_dcs 1 'independent private DC identities'
Require $extra.first.stretch 3 'first private mode'
Require $extra.second.stretch 4 'second private mode'
Require $extra.first.text_color 0x030201 'first private text color'
Require $extra.second.text_color 0x060504 'second private text color'
Require $extra.window_dc_matches_private 0 'GetWindowDC uses a separate common DC'
Require-State $extra.window_dc $cases[0].initial 'GetWindowDC default attributes'
Require-State $extra.repeated_window_dc $cases[0].initial 'GetWindowDC resets attributes'
Require-State $extra.private_after_window_dc $extra.first 'GetWindowDC preserves private DC'
Require-State $extra.second_after_window_dc $extra.second 'other private DC is independent'
Require $extra.destroyed_first_mode 0 'first private DC retires independently'
Require $extra.destroyed_first_release 0 'first retired DC release'
Require $extra.destroyed_first_release_error 1425 'first retired DC release error'
Require-State $extra.second_after_first_destroy $extra.second 'surviving private DC retains attributes'
Require $extra.destroyed_second_mode 0 'second private DC retires independently'
[ordered]@{ cases = $cases; independent = $extra } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'results.json')
'Exit code: 0; native assertions: passed' | Add-Content -LiteralPath (Join-Path $OutputDirectory 'cmd.txt')
'native window DC lifecycle assertions passed'
