param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures/d3d9-stretch-native'),
    [string]$SdkDirectory = (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'),
    [string]$SdkVersion = '10.0.26100.0',
    [switch]$Run,
    [switch]$Reference
)
$ErrorActionPreference = 'Stop'
$nativeDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$nativeSource = Join-Path $PSScriptRoot 'd3d9-stretch-native.c'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
& clang-cl /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$SdkDirectory/Include/$SdkVersion/um" "/I$SdkDirectory/Include/$SdkVersion/shared" "/I$SdkDirectory/Include/$SdkVersion/ucrt" $nativeSource "/Fo$nativeDirectory/d3d9-stretch-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'Native StretchRect compile failed' }
& lld-link /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$SdkDirectory/Lib/$SdkVersion/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$nativeDirectory/d3d9-stretch-native.exe" "$nativeDirectory/d3d9-stretch-native.obj" kernel32.lib user32.lib d3d9.lib
if ($LASTEXITCODE -ne 0) { throw 'Native StretchRect link failed' }
@{
    architecture = 'x86'; sdk = $SdkVersion
    sourceSha256 = (Get-FileHash -LiteralPath $nativeSource -Algorithm SHA256).Hash
    exeSha256 = (Get-FileHash -LiteralPath "$nativeDirectory/d3d9-stretch-native.exe" -Algorithm SHA256).Hash
} | ConvertTo-Json | Set-Content -LiteralPath "$nativeDirectory/environment.json" -Encoding utf8
if ($Run) {
    $nativeArgument = if ($Reference) { '-ref' } else { '-hal' }
    $nativeProcess = Start-Process -FilePath "$nativeDirectory/d3d9-stretch-native.exe" -ArgumentList $nativeArgument -WorkingDirectory $nativeDirectory -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput "$nativeDirectory/native.jsonl" -RedirectStandardError "$nativeDirectory/native.stderr.txt"
    if ($nativeProcess.ExitCode -ne 0) { throw "Native StretchRect oracle exited $($nativeProcess.ExitCode)" }
    $rows = @(Get-Content "$nativeDirectory/native.jsonl" | ForEach-Object { $_ | ConvertFrom-Json })
    if (@($rows | Where-Object op -eq 'case').Count -ne 12) { throw 'Incomplete native StretchRect results' }
    $failures = @($rows | Where-Object { $_.op -eq 'case' -and $_.hr -ne '00000000' }).Count
    Write-Output "Native StretchRect: 12 case rows, $failures driver-rejected profiles; $nativeDirectory/native.jsonl"
}
