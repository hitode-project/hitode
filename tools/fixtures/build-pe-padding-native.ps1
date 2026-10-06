param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/engine-loop/20261006-himawari/pe-padding-native'),
    [switch]$Run
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:/Program Files/LLVM/bin'
$sdk = 'C:/Program Files (x86)/Windows Kits/10'
$version = '10.0.26100.0'
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'pe-padding-native.c') "/Fo$OutputDirectory/pe-padding-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'PE padding native compile failed' }
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/pe-padding-native.exe" "$OutputDirectory/pe-padding-native.obj" kernel32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'PE padding native link failed' }
if ($Run) {
    $nativeProcess = Start-Process -FilePath (Join-Path $OutputDirectory 'pe-padding-native.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput (Join-Path $OutputDirectory 'native.jsonl') -RedirectStandardError (Join-Path $OutputDirectory 'native.stderr.txt')
    if ($nativeProcess.ExitCode -ne 0) { throw "PE padding native oracle exited $($nativeProcess.ExitCode)" }
    $nativeRows = @(Get-Content -LiteralPath (Join-Path $OutputDirectory 'native.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
    if ($nativeRows.Count -eq 0 -or $nativeRows[-1].case -ne 'complete') { throw 'PE padding native oracle did not complete' }
    for ($nativeIndex = 0; $nativeIndex -lt $nativeRows.Count; ++$nativeIndex) {
        if ($nativeRows[$nativeIndex].seq -ne $nativeIndex + 1) { throw 'PE padding native sequence mismatch' }
    }
    $nativeRows | Where-Object { $_.op -eq 'load' } | Format-Table -AutoSize
    "PE padding native oracle: $($nativeRows.Count) valid JSONL records"
}
