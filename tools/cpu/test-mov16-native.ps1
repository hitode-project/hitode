param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/cpu-reference/native'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86'
$object = Join-Path $OutputDirectory 'mov16-native.obj'
$executable = Join-Path $OutputDirectory 'mov16-native.exe'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc (Join-Path $PSScriptRoot 'mov16-native.c') "/Fo$object"
if ($LASTEXITCODE -ne 0) { throw 'MOV16 probe compile failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /safeseh:no /timestamp:0 "/libpath:$sdk" /subsystem:console /entry:mainCRTStartup "/out:$executable" $object kernel32.lib
if ($LASTEXITCODE -ne 0) { throw 'MOV16 probe link failed' }
$process = Start-Process -FilePath $executable -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
if (-not $process.WaitForExit(5000)) {
    $process.Kill()
    $process.WaitForExit(2000) | Out-Null
    throw 'MOV16 native probe timed out'
}
if ($process.ExitCode -ne 0) { throw "MOV16 native probe failed: $($process.ExitCode)" }
'256 WoW64 word-move cases passed; upper words and flags preserved. IDIV overflow raised 0xc0000095.'
