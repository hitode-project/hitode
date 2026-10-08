<#
.SYNOPSIS
Build the x86 collector, launcher and owned fixture with installed LLVM/SDK.
.DESCRIPTION
Input: vendored MinHook and signatures.tsv. Output: DLL/EXEs, generated coverage,
objects in OutputDirectory. Temporary compiler files stay in that directory.
#>
param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$LlvmBin = 'C:/Program Files/LLVM/bin',
    [string]$WindowsSdk = 'C:/Program Files (x86)/Windows Kits/10',
    [string]$SdkVersion = '10.0.26100.0',
    [string]$VcRuntimeDirectory
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$savedTemp = $env:TEMP
$savedTmp = $env:TMP
$savedBytecode = $env:PYTHONDONTWRITEBYTECODE
$env:TEMP = $OutputDirectory
$env:TMP = $OutputDirectory
$env:PYTHONDONTWRITEBYTECODE = '1'
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Build failed: $Program exit $LASTEXITCODE" }
}
try {
    if (!$VcRuntimeDirectory) {
        $vswhere = 'C:/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe'
        $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (!$vsRoot) { throw 'Visual Studio x86 runtime library is required for compiler stack probes' }
        $vcVersion = (Get-Content -LiteralPath (Join-Path $vsRoot 'VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt')).Trim()
        $VcRuntimeDirectory = Join-Path $vsRoot "VC/Tools/MSVC/$vcVersion/lib/x86"
    }
    $vendor = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../fixtures/vendor/minhook'))
    foreach ($line in Get-Content -LiteralPath (Join-Path $vendor 'SHA256SUMS')) {
        $hash, $relative = $line -split '  ', 2
        if ((Get-FileHash -LiteralPath (Join-Path $vendor $relative) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
            throw "Pinned MinHook hash mismatch: $relative"
        }
    }
    Invoke-Checked 'python' @((Join-Path $PSScriptRoot 'generate_hooks.py'), '--output', "$OutputDirectory/hooks.inc")
    $compiler = Join-Path $LlvmBin 'clang-cl.exe'
    $common = @('/nologo', '/c', '/O2', '/GS-', '/Zl', '/clang:-fno-builtin',
        '/clang:--target=i686-pc-windows-msvc', "/I$OutputDirectory", "/I$vendor/include",
        "/I$WindowsSdk/Include/$SdkVersion/um", "/I$WindowsSdk/Include/$SdkVersion/shared",
        "/I$WindowsSdk/Include/$SdkVersion/ucrt")
    $sources = @((Join-Path $PSScriptRoot 'native_trace.c'), (Join-Path $PSScriptRoot 'native_capture.c'),
        (Join-Path $PSScriptRoot 'fixture.c'), (Join-Path $PSScriptRoot '../fixtures/patch_crt.c'),
        "$vendor/src/buffer.c", "$vendor/src/hook.c", "$vendor/src/trampoline.c", "$vendor/src/hde/hde32.c")
    foreach ($source in $sources) {
        $name = [IO.Path]::GetFileNameWithoutExtension($source)
        Invoke-Checked $compiler ($common + @($source, "/Fo$OutputDirectory/$name.obj"))
    }
    $linker = Join-Path $LlvmBin 'lld-link.exe'
    $base = @('/nologo', '/machine:x86', '/nodefaultlib', '/dynamicbase', '/safeseh:no', '/timestamp:0',
        "/libpath:$WindowsSdk/Lib/$SdkVersion/um/x86", "/libpath:$VcRuntimeDirectory")
    Invoke-Checked $linker ($base + @('/dll', '/entry:DllMain@12', '/export:TraceInitialize=_TraceInitialize@4',
        "/out:$OutputDirectory/native_trace.dll", "$OutputDirectory/native_trace.obj", "$OutputDirectory/patch_crt.obj",
        "$OutputDirectory/buffer.obj", "$OutputDirectory/hook.obj", "$OutputDirectory/trampoline.obj", "$OutputDirectory/hde32.obj", 'kernel32.lib', 'libcmt.lib'))
    Invoke-Checked $linker ($base + @('/subsystem:console', '/entry:mainCRTStartup',
        "/out:$OutputDirectory/native_capture.exe", "$OutputDirectory/native_capture.obj", "$OutputDirectory/patch_crt.obj", 'kernel32.lib', 'shell32.lib', 'libcmt.lib'))
    Invoke-Checked $linker ($base + @('/subsystem:console', '/entry:mainCRTStartup',
        "/out:$OutputDirectory/api_trace_fixture.exe", "$OutputDirectory/fixture.obj", 'kernel32.lib', 'libcmt.lib'))
    Write-Output 'Built x86 collector, launcher and fixture'
} finally {
    $env:TEMP = $savedTemp
    $env:TMP = $savedTmp
    $env:PYTHONDONTWRITEBYTECODE = $savedBytecode
}
