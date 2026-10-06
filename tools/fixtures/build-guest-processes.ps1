param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/guest-processes'),
    [switch]$RunNative
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$processLlvm = 'C:/Program Files/LLVM/bin'
$processSdk = 'C:/Program Files (x86)/Windows Kits/10'
$processVersion = '10.0.26100.0'
foreach ($processRole in @('parent', 'child')) {
    $processDefine = if ($processRole -eq 'child') { '/DCHILD=1' } else { '/DPARENT=1' }
    & (Join-Path $processLlvm 'clang-cl.exe') /nologo /c /Od /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc $processDefine "/I$processSdk/Include/$processVersion/um" "/I$processSdk/Include/$processVersion/shared" (Join-Path $PSScriptRoot 'guest_processes.c') "/Fo$OutputDirectory/guest_$processRole.obj"
    if ($LASTEXITCODE -ne 0) { throw "Owned guest process $processRole compile failed" }
    & (Join-Path $processLlvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase:no /base:0x400000 /fixed:no /opt:noref /opt:noicf /safeseh:no /timestamp:0 "/libpath:$processSdk/Lib/$processVersion/um/x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_guest_$processRole.exe" "$OutputDirectory/guest_$processRole.obj" kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw "Owned guest process $processRole link failed" }
}
if ($RunNative) {
    # Remove only the three named outputs from the explicitly chosen directory.
    foreach ($processOutputName in @('process-parent.bin', 'process-child.bin', 'process-child-late.bin')) {
        $processOutputPath = Join-Path $OutputDirectory $processOutputName
        if (Test-Path -LiteralPath $processOutputPath) { Remove-Item -LiteralPath $processOutputPath }
    }
    $processParent = Start-Process -FilePath (Join-Path $OutputDirectory 'fixture_guest_parent.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
    try {
        if (!$processParent.WaitForExit(10000)) { throw 'Owned guest process parent timed out' }
        if ($processParent.ExitCode -ne 0) { throw "Owned guest process parent failed: $($processParent.ExitCode)" }
    } finally {
        if (!$processParent.HasExited) { $processParent.Kill(); $null = $processParent.WaitForExit(1000) }
        $processParent.Dispose()
    }
    $processDeadline = [DateTime]::UtcNow.AddSeconds(5)
    $processLatePath = Join-Path $OutputDirectory 'process-child-late.bin'
    while (!(Test-Path -LiteralPath $processLatePath) -and [DateTime]::UtcNow -lt $processDeadline) { Start-Sleep -Milliseconds 50 }
    foreach ($processTraceName in @('process-parent.bin', 'process-child.bin', 'process-child-late.bin')) {
        $processTrace = [IO.File]::ReadAllBytes((Join-Path $OutputDirectory $processTraceName))
        $processExpectedLength = if ($processTraceName -eq 'process-parent.bin') { 256 } else { 32 }
        if ($processTrace.Length -ne $processExpectedLength) { throw "Unexpected $processTraceName length $($processTrace.Length)" }
        $processWords = for ($processIndex = 0; $processIndex -lt $processTrace.Length / 4; $processIndex++) { [BitConverter]::ToUInt32($processTrace, $processIndex * 4) }
        "$processTraceName $($processWords -join ' ')"
    }
}
