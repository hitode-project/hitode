param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-patch'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$vendor = Join-Path $PSScriptRoot 'vendor/minhook'
foreach ($line in Get-Content -LiteralPath (Join-Path $vendor 'SHA256SUMS')) {
    $hash, $relative = $line -split '  ', 2
    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead((Join-Path $vendor $relative))
    try {
        $actual = [System.BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
    } finally {
        $stream.Dispose()
        $algorithm.Dispose()
    }
    if ($actual -ne $hash) { throw "Pinned MinHook source hash mismatch: $relative" }
}
function Invoke-PatchChecked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
$compiler = Join-Path $LlvmBin 'clang-cl.exe'
$common = @('/nologo', '/c', '/O2', '/GS-', '/Gs9999999', '/Zl', '/clang:--target=i686-pc-windows-msvc', "/I$PSScriptRoot/patch_headers", "/I$PSScriptRoot")
$sources = @('patch_compat.c', 'patch_crt.c', 'patch_dllmain.c', 'vendor/minhook/src/buffer.c', 'vendor/minhook/src/hook.c', 'vendor/minhook/src/trampoline.c', 'vendor/minhook/src/hde/hde32.c')
foreach ($source in $sources) {
    $name = [System.IO.Path]::GetFileNameWithoutExtension($source)
    Invoke-PatchChecked $compiler ($common + @((Join-Path $PSScriptRoot $source), "/Fo$OutputDirectory/$name.obj"))
}
Invoke-PatchChecked (Join-Path $LlvmBin 'clang.exe') @('--target=i686-pc-windows-msvc', '-c', (Join-Path $PSScriptRoot 'patch_target.s'), '-o', "$OutputDirectory/patch_target.obj")
$base = @('/nologo', '/machine:x86', '/nodefaultlib', '/dynamicbase', '/base:0x400000', '/safeseh:no', '/timestamp:0', '/manifest:embed', "/manifestuac:level='asInvoker' uiAccess='false'", "/libpath:$WindowsSdk")
$linker = Join-Path $LlvmBin 'lld-link.exe'
Invoke-PatchChecked $linker ($base + @('/dll', '/entry:DllMain@12', "/out:$OutputDirectory/fixture_patch_dllmain.dll", "$OutputDirectory/patch_dllmain.obj", 'kernel32.lib'))
Invoke-PatchChecked $linker ($base + @('/subsystem:console', '/entry:mainCRTStartup', "/def:$PSScriptRoot/patch_compat.def", "/out:$OutputDirectory/fixture_patch_compat.exe", "$OutputDirectory/patch_compat.obj", "$OutputDirectory/patch_crt.obj", "$OutputDirectory/patch_target.obj", "$OutputDirectory/buffer.obj", "$OutputDirectory/hook.obj", "$OutputDirectory/trampoline.obj", "$OutputDirectory/hde32.obj", 'kernel32.lib', 'ddraw.lib'))
Write-Output "Built x86 patch fixtures in $OutputDirectory"
