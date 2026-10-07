param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/kerning-pairs-native'),
    [string]$LlvmBin = 'C:\Program Files\LLVM\bin',
    [string]$WindowsSdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
& (Join-Path $LlvmBin 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'kerning-pairs-native.c') "/Fo$OutputDirectory/kerning-pairs-native.obj"
if ($LASTEXITCODE -ne 0) { throw 'Kerning pairs native compile failed' }
& (Join-Path $LlvmBin 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$WindowsSdk" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/kerning-pairs-native.exe" "$OutputDirectory/kerning-pairs-native.obj" kernel32.lib gdi32.lib
if ($LASTEXITCODE -ne 0) { throw 'Kerning pairs native link failed' }
$process = Start-Process -FilePath (Join-Path $OutputDirectory 'kerning-pairs-native.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Kerning pairs native failed: $($process.ExitCode)" }
$bytes = [IO.File]::ReadAllBytes((Join-Path $OutputDirectory 'kerning-pairs-native.bin'))
$values = for ($index = 0; $index -lt $bytes.Length; $index += 4) { [BitConverter]::ToUInt32($bytes, $index) }
Write-Output ($values -join ',')
