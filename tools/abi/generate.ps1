param(
    [string]$Winmd,
    [switch]$Download,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$version = '71.0.26-preview'
$packageHash = '758EFE32666596B8596C58A8C46631DA23757A914713B4E88403BBFA5110CB38'
$winmdHash = '0C0E5F11543FD654F9FAD44C1952494BF407D6B067D93C2E5EEF4FCF5D42EDDE'
$cache = Join-Path $repoRoot 'target/tmp/abi-metadata'
if ($Download) {
    New-Item -ItemType Directory -Force $cache | Out-Null
    $archive = Join-Path $cache 'metadata.zip'
    if (!(Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.win32metadata/$version/microsoft.windows.sdk.win32metadata.$version.nupkg" -OutFile $archive
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $packageHash) { throw 'Win32Metadata package hash mismatch' }
    $package = Join-Path $cache 'package'
    Expand-Archive -LiteralPath $archive -DestinationPath $package -Force
    $Winmd = Join-Path $package 'Windows.Win32.winmd'
}
if (!$Winmd) { throw 'Pass -Winmd <Windows.Win32.winmd> or -Download' }
if ((Get-FileHash -LiteralPath $Winmd -Algorithm SHA256).Hash -ne $winmdHash) { throw 'Windows.Win32.winmd hash mismatch' }
Add-Type -Path (Join-Path $PSScriptRoot 'Metadata.cs')
$destination = Join-Path $repoRoot 'data/abi'
if ($Check) { $destination = Join-Path $cache 'check' }
[AbiMetadata]::Generate($Winmd, $destination, (Join-Path $repoRoot 'data/abi/modules.txt'))
if ($Check) {
    foreach ($name in @('signatures.tsv', 'structs.rs', 'com.tsv', 'constants.tsv')) {
        $expected = [IO.File]::ReadAllBytes((Join-Path $repoRoot "data/abi/$name"))
        $actual = [IO.File]::ReadAllBytes((Join-Path $destination $name))
        if (![Linq.Enumerable]::SequenceEqual[byte]($expected, $actual)) { throw "Generated file differs: $name" }
    }
    Write-Output 'ABI generated data matches pinned WinMD.'
}
