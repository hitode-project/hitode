param(
    [string]$Runner = (Join-Path $PSScriptRoot '../../target/debug/adv-run.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/architecture-acceptance/window-placement'),
    [switch]$NativeOnly
)
$ErrorActionPreference = 'Stop'
$env:ADV_AUDIO_MUTE_DEVICE = '1'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$allowed = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
if (-not $OutputDirectory.StartsWith($allowed + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Window placement evidence must stay inside target/architecture-acceptance'
}
& (Join-Path $PSScriptRoot 'build-window-placement.ps1') -OutputDirectory $OutputDirectory
$fixture = Join-Path $OutputDirectory 'fixture_window_placement.exe'
$output = Join-Path $OutputDirectory 'window-placement.bin'
if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Force }
$ownedProbe = Start-Process -FilePath $fixture -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
try {
    if (-not $ownedProbe.WaitForExit(5000)) { throw 'Owned window placement probe exceeded five seconds' }
    if ($ownedProbe.ExitCode -ne 0) { throw "Owned placement probe exited $($ownedProbe.ExitCode)" }
} finally {
    if (-not $ownedProbe.HasExited) { $ownedProbe.Kill() }
    $ownedProbe.Dispose()
}
$native = [IO.File]::ReadAllBytes($output)
if ($native.Length -ne 1196 -or [BitConverter]::ToUInt32($native, 1192) -ne 0x504c0001) { throw 'Incomplete native placement evidence' }
[IO.File]::WriteAllBytes((Join-Path $OutputDirectory 'native.bin'), $native)
Write-Output 'native: 20 full placements; six length hints; NULL, invalid, retired HWND, read-only and crossing-page failures'
if ($NativeOnly) { return }
foreach ($backend in @('interpreter', 'jit')) {
    $folder = Join-Path $OutputDirectory $backend
    $overlay = Join-Path $folder 'data/overlay/window-placement.bin'
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    if (Test-Path -LiteralPath $overlay) { Remove-Item -LiteralPath $overlay -Force }
    $report = Join-Path $folder 'report.json'
    & $Runner $fixture --headless --backend $backend --seconds 5 --data-dir (Join-Path $folder 'data') --report $report --trace (Join-Path $folder 'api.log') *> (Join-Path $folder 'run.log')
    if ($LASTEXITCODE -ne 0) { throw "Placement $backend runner failed: $folder/run.log" }
    $guest = [IO.File]::ReadAllBytes($overlay)
    [IO.File]::WriteAllBytes((Join-Path $folder 'guest.bin'), $guest)
    if ($guest.Length -ne $native.Length) { throw 'Incomplete guest placement evidence' }
    $normalized = 0
    for ($index = 0; $index -lt 299; ++$index) {
        $expected = [BitConverter]::ToUInt32($native, 4 * $index)
        $actual = [BitConverter]::ToUInt32($guest, 4 * $index)
        if ($index -lt 260) {
            $row = [int][Math]::Floor($index / 13)
            $column = $index % 13
            # The host may scale or move its taskbar. Guest UI uses one 1920x1080
            # monitor, a 40px bottom taskbar and 29px minimized fixed-frame caption.
            if ($column -eq 6 -and (($row -ge 4 -and $row -le 9) -or $row -eq 12 -or $row -eq 14 -or $row -eq 19)) {
                if ($expected -eq [uint32]::MaxValue) { throw 'Native probe did not minimize its owned tool window' }
                $expected = 1011
                ++$normalized
            } elseif ($row -eq 13 -and $column -eq 6) {
                $expected = 972 # Maximized parent's 1001px client height minus minimized caption.
                ++$normalized
            }
        }
        if ($actual -ne $expected) { throw ('Placement {0} word {1}: {2:X8}, expected {3:X8}' -f $backend, $index, $actual, $expected) }
    }
    $result = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
    if ($result.api.acceptance_eligible -ne $true -or $result.guest_exit_code -ne 0) { throw 'Placement strict acceptance failed' }
    Write-Output "$backend`: $(299 - $normalized) exact native DWORDs + $normalized virtual-profile icon coordinates; strict guest exit 0"
}
