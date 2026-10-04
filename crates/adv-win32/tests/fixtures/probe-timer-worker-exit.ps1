# Deliberately manual: this misuse probe must never run as an unbounded native
# integration test. It compiles only our source, launches hidden own children,
# waits at most three seconds each, and terminates only those process handles.
param(
    [string]$OutputDirectory = '',
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$env:ADV_AUDIO_MUTE_DEVICE = '1'
$timerRepo = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../../..'))
if ($OutputDirectory -eq '') { $OutputDirectory = Join-Path $timerRepo 'target/timer-worker-exit' }
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
foreach ($mode in @('async', 'sync')) {
    $flag = if ($mode -eq 'sync') { 1 } else { 0 }
    $objectPath = Join-Path $OutputDirectory "$mode.obj"
    $executable = Join-Path $OutputDirectory "$mode.exe"
    & (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc "/I$timerRepo/tools/fixtures" "/DSYNC=$flag" (Join-Path $PSScriptRoot 'timer_worker_exit.c') "/Fo$objectPath"
    if ($LASTEXITCODE -ne 0) { throw 'Owned timer probe compile failed' }
    & (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:console /entry:mainCRTStartup "/out:$executable" $objectPath kernel32.lib winmm.lib
    if ($LASTEXITCODE -ne 0) { throw 'Owned timer probe link failed' }
    $directory = Join-Path $OutputDirectory $mode
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    $report = Join-Path $directory 'report.bin'
    if (Test-Path -LiteralPath $report) { Remove-Item -LiteralPath $report -Force }
    $child = Start-Process -FilePath $executable -WorkingDirectory $directory -WindowStyle Hidden -PassThru
    try {
        $completed = $child.WaitForExit(3000)
        if (-not $completed) {
            $child.Kill()
            if (-not $child.WaitForExit(1000)) { throw 'Owned child did not exit after termination' }
        }
        $words = @()
        if (Test-Path -LiteralPath $report) {
            $bytes = [System.IO.File]::ReadAllBytes($report)
            for ($index = 0; $index + 4 -le $bytes.Length; $index += 4) {
                $words += [System.BitConverter]::ToUInt32($bytes, $index)
            }
        }
        [pscustomobject]@{ Mode = $mode; TimedOut = -not $completed; Words = ($words -join ','); ExitCode = $child.ExitCode }
    } finally {
        if (-not $child.HasExited) {
            $child.Kill()
            if (-not $child.WaitForExit(1000)) { throw 'Owned child cleanup did not complete' }
        }
        $child.Dispose()
    }
}
