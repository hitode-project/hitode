param([ValidateSet('majiro', 'nana')][string]$Game)
$ErrorActionPreference = 'Stop'
try {
    $games = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'games.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not $Game) {
        Write-Host '1. Katahane / 2. Nanatsuiro'
        $choice = Read-Host 'Select 1 or 2'
        if ($choice -eq '1') { $Game = 'majiro' }
        elseif ($choice -eq '2') { $Game = 'nana' }
        else { throw 'Select 1 or 2.' }
    }
    $entry = $games.$Game
    if (-not $entry -or -not (Test-Path -LiteralPath $entry.exe -PathType Leaf)) {
        throw 'Game executable not found. Edit games.json to set its full path.'
    }
    $data = Join-Path $PSScriptRoot "data/$Game"
    New-Item -ItemType Directory -Force -Path $data | Out-Null
    $report = Join-Path $data ('session-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
    & (Join-Path $PSScriptRoot 'adv-run.exe') --exe $entry.exe --codepage $entry.codepage --backend jit --guest-workers 1 --compile fast --codec native --codec-kernels native --api-mode strict --audio --data-dir $data --report $report
    if ($LASTEXITCODE -ne 0) { throw "ADV exited with code $LASTEXITCODE. Report: $report" }
} catch {
    Write-Host $_ -ForegroundColor Red
    Read-Host 'Press Enter to close'
    exit 1
}
