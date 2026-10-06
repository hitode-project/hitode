param(
    [string]$DetoursDirectory = (Join-Path $PSScriptRoot '../../target/process-reference/Detours'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/detours-processes'),
    [switch]$RestoreAfterWith,
    [switch]$RunNative
)
$ErrorActionPreference = 'Stop'
$DetoursDirectory = [IO.Path]::GetFullPath($DetoursDirectory)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (!(Test-Path -LiteralPath (Join-Path $DetoursDirectory 'src/creatwth.cpp'))) { throw 'Supply the upstream Microsoft Detours checkout with -DetoursDirectory' }
$detoursPinnedHead = & git -C $DetoursDirectory rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot read the upstream Detours revision' }
$detoursSourceChanges = & git -C $DetoursDirectory diff --name-only HEAD -- src
if ($detoursSourceChanges) { throw 'Detours src must remain unmodified for this differential fixture' }
$detoursVsWhere = 'C:/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe'
$detoursVsRoot = & $detoursVsWhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$detoursVsRoot) { throw 'Visual Studio x86 C++ Build Tools are required to build the unmodified upstream library' }
$detoursVsDevCmd = Join-Path $detoursVsRoot 'Common7/Tools/VsDevCmd.bat'
$detoursEnvironment = & cmd.exe /d /c "call `"$detoursVsDevCmd`" -no_logo -arch=x86 -host_arch=x64 && set"
if ($LASTEXITCODE -ne 0) { throw 'Visual Studio x86 environment setup failed' }
foreach ($detoursLine in $detoursEnvironment) {
    if ($detoursLine -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process') }
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
Push-Location (Join-Path $DetoursDirectory 'src')
try {
    & nmake.exe /nologo DETOURS_TARGET_PROCESSOR=X86
    if ($LASTEXITCODE -ne 0) { throw 'Upstream Detours nmake library build failed' }
} finally { Pop-Location }
$detoursLibrary = Join-Path $DetoursDirectory 'lib.X86/detours.lib'
$detoursCommon = @('/nologo', '/c', '/Od', '/GS-', '/Gs9999999', '/Zl', "/I$DetoursDirectory/include")
& cl.exe @detoursCommon (Join-Path $PSScriptRoot 'detours_processes_crt.cpp') "/Fo$OutputDirectory/detours_crt.obj"
if ($LASTEXITCODE -ne 0) { throw 'Owned Detours CRT support compile failed' }
& cl.exe @detoursCommon (Join-Path $PSScriptRoot 'detours_processes_child.c') "/Fo$OutputDirectory/detours_child.obj"
if ($LASTEXITCODE -ne 0) { throw 'Owned Detours child compile failed' }
& link.exe /nologo /machine:x86 /nodefaultlib /dynamicbase:no /base:0x400000 /fixed:no /opt:ref /safeseh:no /Brepro /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_detours_child.exe" "$OutputDirectory/detours_child.obj" kernel32.lib
if ($LASTEXITCODE -ne 0) { throw 'Owned Detours child link failed' }
[string[]]$detoursDllDefines = if ($RestoreAfterWith) { @('/DRESTORE_AFTER_WITH=1') } else { @() }
& cl.exe @detoursCommon @detoursDllDefines (Join-Path $PSScriptRoot 'detours_processes_dll.cpp') "/Fo$OutputDirectory/detours_dll.obj"
if ($LASTEXITCODE -ne 0) { throw 'Owned Detours DLL compile failed' }
$detoursRuntimeLibraries = @('libcmt.lib', 'libvcruntime.lib', 'libucrt.lib')
& link.exe /nologo /machine:x86 /nodefaultlib /dll /dynamicbase:no /base:0x68000000 /fixed:no /opt:ref /safeseh:no /Brepro /entry:DllMain@12 '/export:InjectedOrdinalOne=_InjectedOrdinalOne@0,@1' "/out:$OutputDirectory/fixture_detours_injected.dll" "$OutputDirectory/detours_dll.obj" "$OutputDirectory/detours_crt.obj" $detoursLibrary @detoursRuntimeLibraries kernel32.lib
if ($LASTEXITCODE -ne 0) { throw 'Owned Detours DLL link failed' }
foreach ($detoursVariant in @('a', 'w', 'as', 'ws')) {
    $detoursDefines = @()
    if ($detoursVariant.StartsWith('w')) { $detoursDefines += '/DWIDE_PROCESS=1' }
    if ($detoursVariant.EndsWith('s')) { $detoursDefines += '/DSUSPENDED_PROCESS=1' }
    & cl.exe @detoursCommon @detoursDefines (Join-Path $PSScriptRoot 'detours_processes_parent.cpp') "/Fo$OutputDirectory/detours_parent_$detoursVariant.obj"
    if ($LASTEXITCODE -ne 0) { throw "Owned Detours $detoursVariant parent compile failed" }
    & link.exe /nologo /machine:x86 /nodefaultlib /dynamicbase:no /base:0x400000 /fixed:no /opt:ref /safeseh:no /Brepro /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_detours_parent_$detoursVariant.exe" "$OutputDirectory/detours_parent_$detoursVariant.obj" "$OutputDirectory/detours_crt.obj" $detoursLibrary @detoursRuntimeLibraries kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw "Owned Detours $detoursVariant parent link failed" }
}
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'detours-upstream-revision.txt'), "$detoursPinnedHead`n")
"Built actual upstream Detours $detoursPinnedHead in $OutputDirectory"
if ($RunNative) {
    foreach ($detoursVariant in @('a', 'w', 'as', 'ws')) {
        foreach ($detoursKind in @('parent', 'child')) {
            $detoursOutput = Join-Path $OutputDirectory "detours-$detoursKind-$detoursVariant.bin"
            if (Test-Path -LiteralPath $detoursOutput) { Remove-Item -LiteralPath $detoursOutput }
        }
        $detoursParent = Start-Process -FilePath (Join-Path $OutputDirectory "fixture_detours_parent_$detoursVariant.exe") -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
        $detoursNativeSucceeded = $false
        try {
            if (!$detoursParent.WaitForExit(10000)) { throw "Owned native Detours $detoursVariant parent timed out" }
            if ($detoursParent.ExitCode -ne 0) { throw "Owned native Detours $detoursVariant parent exited $($detoursParent.ExitCode)" }
            $detoursNativeSucceeded = $true
        } finally {
            if (!$detoursParent.HasExited) { $detoursParent.Kill(); $null = $detoursParent.WaitForExit(1000) }
            # Failed injection can leave an owned native child suspended. Limit
            # cleanup to the fixture child created by this exact parent PID.
            if (!$detoursNativeSucceeded) {
                Get-CimInstance Win32_Process -Filter "Name='fixture_detours_child.exe'" |
                    Where-Object { $_.ParentProcessId -eq $detoursParent.Id } |
                    ForEach-Object {
                        $detoursOwnedChild = Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
                        if ($detoursOwnedChild) { Stop-Process -InputObject $detoursOwnedChild -Force; $null = $detoursOwnedChild.WaitForExit(1000) }
                    }
            }
            $detoursParent.Dispose()
        }
        $detoursChildOutput = Join-Path $OutputDirectory "detours-child-$detoursVariant.bin"
        $detoursDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while (!(Test-Path -LiteralPath $detoursChildOutput) -and [DateTime]::UtcNow -lt $detoursDeadline) { Start-Sleep -Milliseconds 50 }
        foreach ($detoursKind in @('parent', 'child')) {
            $detoursTraceName = "detours-$detoursKind-$detoursVariant.bin"
            $detoursTrace = [IO.File]::ReadAllBytes((Join-Path $OutputDirectory $detoursTraceName))
            if ($detoursTrace.Length -ne 32) { throw "Unexpected $detoursTraceName length $($detoursTrace.Length)" }
            $detoursWords = for ($detoursIndex = 0; $detoursIndex -lt 8; $detoursIndex++) { [BitConverter]::ToUInt32($detoursTrace, $detoursIndex * 4) }
            if ($detoursKind -eq 'parent') {
                $detoursExpectedWide = [int]$detoursVariant.StartsWith('w')
                $detoursExpectedSuspended = [int]$detoursVariant.EndsWith('s')
                $detoursExpectedWords = @(0x44505431, 1, $detoursExpectedWide, $detoursExpectedSuspended, 1, 0, $detoursExpectedSuspended, 1)
            } else {
                $detoursExpectedVariant = [Array]::IndexOf(@('a', 'w', 'as', 'ws'), $detoursVariant)
                $detoursExpectedWords = @(0x44435431, 1, $detoursExpectedVariant, 1, 0, 1, 0x400000, 1)
            }
            for ($detoursIndex = 0; $detoursIndex -lt 8; $detoursIndex++) {
                if ($detoursWords[$detoursIndex] -ne $detoursExpectedWords[$detoursIndex]) { throw "$detoursTraceName word $detoursIndex expected $($detoursExpectedWords[$detoursIndex]), got $($detoursWords[$detoursIndex])" }
            }
            "$detoursTraceName $($detoursWords -join ' ')"
        }
    }
}
