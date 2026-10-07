# Build a self-contained Windows playtest folder: release runner (GPU, and movie
# decoding when an LGPL FFmpeg 8 root is given), its FFmpeg DLLs and the launcher.
# -Games copies a local preset list (games.json, see launcher.ps1) next to it.
# Output goes outside the source tree's tracked files; nothing here is committed.
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/playtest/adv-playtest'),
    [string]$TargetDirectory = (Join-Path $PSScriptRoot '../../target'),
    [string]$FfmpegDirectory = $env:FFMPEG_DIR,
    [string]$Games
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$TargetDirectory = [IO.Path]::GetFullPath($TargetDirectory)
$features = 'adv-shell/gpu'
if ($FfmpegDirectory) {
    $env:FFMPEG_DIR = [IO.Path]::GetFullPath($FfmpegDirectory)
    if (-not $env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin' }
    $features += ',adv-shell/media'
} else {
    Write-Warning 'No FFmpeg root (-FfmpegDirectory or FFMPEG_DIR): movies will be unavailable.'
}
Push-Location $root
try {
    & cargo build --release -p adv-run --features $features --target-dir $TargetDirectory
    if ($LASTEXITCODE -ne 0) { throw 'runner build failed' }
} finally { Pop-Location }

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
Copy-Item -Force (Join-Path $TargetDirectory 'release/adv-run.exe') $OutputDirectory
if ($FfmpegDirectory) {
    foreach ($dll in Get-ChildItem (Join-Path $env:FFMPEG_DIR 'bin') -Filter '*.dll') {
        if ($dll.Name -match '^(avcodec|avformat|avutil|swresample|swscale)-\d+\.dll$') {
            Copy-Item -Force $dll.FullName $OutputDirectory
        }
    }
}
Copy-Item -Force (Join-Path $PSScriptRoot 'launcher.ps1') $OutputDirectory
Copy-Item -Force (Join-Path $PSScriptRoot 'README.txt') $OutputDirectory
if ($Games) {
    $null = Get-Content -LiteralPath $Games -Raw -Encoding UTF8 | ConvertFrom-Json
    Copy-Item -Force -LiteralPath $Games (Join-Path $OutputDirectory 'games.json')
}
Set-Content -Path (Join-Path $OutputDirectory 'Play.cmd') -Encoding ASCII -Value @(
    '@echo off',
    'start "" powershell.exe -NoProfile -ExecutionPolicy Bypass -STA -WindowStyle Hidden -File "%~dp0launcher.ps1"'
)
$commit = (& git -C $root rev-parse --short HEAD).Trim()
$dirty = if (& git -C $root status --porcelain --untracked-files=no) { '-dirty' } else { '' }
$stream = [IO.File]::OpenRead((Join-Path $OutputDirectory 'adv-run.exe'))
try {
    $hash = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })
} finally { $stream.Dispose() }
Set-Content -Path (Join-Path $OutputDirectory 'BUILD.txt') -Encoding UTF8 -Value @(
    "commit: $commit$dirty", "features: $features", "adv-run.exe sha256: $hash")
Write-Host "playtest folder: $OutputDirectory ($commit$dirty)"
