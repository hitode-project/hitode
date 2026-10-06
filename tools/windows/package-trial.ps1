param(
    [string]$Executable = 'target/release/adv-run.exe',
    [string]$Output = 'dist/windows/adv-host-windows-trial',
    [string]$Majiro = '<games>\片羽（カタハネ）\カタハネ.exe',
    [string]$Nana = '<games>\七色★星露\nana_chs.exe'
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if (-not [IO.Path]::IsPathRooted($Executable)) { $Executable = Join-Path $repo $Executable }
if (-not [IO.Path]::IsPathRooted($Output)) { $Output = Join-Path $repo $Output }
if (Test-Path -LiteralPath $Output) { throw "Use a fresh package directory: $Output" }
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw "Missing build: $Executable" }
New-Item -ItemType Directory -Path $Output -Force | Out-Null
Copy-Item -LiteralPath $Executable -Destination (Join-Path $Output 'adv-run.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'launch-trial.ps1') -Destination $Output
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'trial-readme.txt') -Destination (Join-Path $Output 'README.txt')
@{ majiro = @{ exe = $Majiro; codepage = 932 }; nana = @{ exe = $Nana; codepage = 936 } } |
    ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $Output 'games.json') -Encoding UTF8
foreach ($game in @('majiro', 'nana')) {
    $batch = "@echo off`r`npowershell.exe -NoProfile -ExecutionPolicy Bypass -File `"%~dp0launch-trial.ps1`" -Game $game`r`n"
    [IO.File]::WriteAllText((Join-Path $Output "play-$game.cmd"), $batch, [Text.Encoding]::ASCII)
}
$revision = git -C $repo rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source revision' }
@{
    source_revision = $revision
    source_changes = @(git -C $repo status --short)
    build_command = 'cargo build --release -p adv-run --features adv-shell/gpu'
    packaged_at = (Get-Date).ToString('o')
    executable_sha256 = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash
} | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $Output 'build-info.json') -Encoding UTF8
Compress-Archive -LiteralPath $Output -DestinationPath "$Output.zip" -CompressionLevel Optimal
Get-Item -LiteralPath "$Output.zip"
