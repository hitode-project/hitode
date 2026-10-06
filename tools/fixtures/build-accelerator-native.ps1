param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/engine-loop/20261006-himawari/accelerator-native'),
    [switch]$Run
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:/Program Files/LLVM/bin'
$sdk = 'C:/Program Files (x86)/Windows Kits/10'
$version = '10.0.26100.0'
& "$sdk/bin/$version/x64/rc.exe" /nologo "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/fo$OutputDirectory/accelerator-native.res" (Join-Path $PSScriptRoot 'accelerator-native.rc')
if ($LASTEXITCODE -ne 0) { throw 'Accelerator native resource compile failed' }
& "$llvm/clang-cl.exe" /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" (Join-Path $PSScriptRoot 'accelerator-native.c') "/Fo$OutputDirectory/accelerator-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'Accelerator native compile failed' }
& "$llvm/lld-link.exe" /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/accelerator-native.exe" "$OutputDirectory/accelerator-native.obj" "$OutputDirectory/accelerator-native.res" kernel32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'Accelerator native link failed' }
if ($Run) {
    $nativeOutput = Join-Path $OutputDirectory 'native.jsonl'
    $nativeError = Join-Path $OutputDirectory 'native.stderr.txt'
    $nativeProcess = Start-Process -FilePath (Join-Path $OutputDirectory 'accelerator-native.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $nativeOutput -RedirectStandardError $nativeError
    if ($nativeProcess.ExitCode -ne 0) { throw "Accelerator native oracle exited $($nativeProcess.ExitCode)" }
    $nativeRows = @(Get-Content -LiteralPath $nativeOutput | ForEach-Object { $_ | ConvertFrom-Json })
    if ($nativeRows.Count -eq 0 -or $nativeRows[-1].op -ne 'complete' -or $nativeRows[-1].result -ne 1) { throw 'Accelerator native oracle did not complete' }
    for ($nativeIndex = 0; $nativeIndex -lt $nativeRows.Count; ++$nativeIndex) {
        if ($nativeRows[$nativeIndex].seq -ne $nativeIndex + 1) { throw 'Accelerator native oracle sequence mismatch' }
    }
    Write-Output "Native accelerator oracle: $($nativeRows.Count) valid JSON records; $nativeOutput"
}
