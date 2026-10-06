param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../../target/fixtures-dialog-unicode'))
$ErrorActionPreference = 'Stop'
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$llvm = 'C:\Program Files\LLVM\bin'
$sdkRoot = 'C:\Program Files (x86)\Windows Kits\10'
$sdkVersion = '10.0.26100.0'
$includes = @("/I$sdkRoot\Include\$sdkVersion\shared", "/I$sdkRoot\Include\$sdkVersion\um", "/I$sdkRoot\Include\$sdkVersion\ucrt")
function Assert-Transcript($Words, [long]$MissingResourceError, [bool]$HasResources) {
    $callback = @(0, 1, 1, 1, 4, 0x6f22, 0x5b57, 0xd83d, 0xde00, 0, 1, 2, 0x96ea, 0xd83d, 0, 1, 2, 45, 55)
    $modalCallback = $callback.Clone()
    $modalCallback[0] = 7
    $expected = $callback + @(1, 1, 0, 0) + $modalCallback + @(0, 1, 1, 4294967289L, 1, 0, 0, 1400, 4294967295L, $MissingResourceError, 4294967295L, 0, 1, 0, 0, 1, 4294967295L, 1)
    if ($HasResources) { $expected += @(0x12345678, 0x87654321L) }
    if ($Words.Count -ne $expected.Count) { throw "Expected $($expected.Count) native records, got $($Words.Count)" }
    for ($entry = 0; $entry -lt $expected.Count; ++$entry) {
        if ([long]$Words[$entry] -ne [long]$expected[$entry]) { throw "Native record $entry differs: expected $($expected[$entry]), got $($Words[$entry])" }
    }
}
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:--target=i686-pc-windows-msvc @includes (Join-Path $PSScriptRoot 'dialog_unicode_native.c') "/Fo$OutputDirectory/dialog_unicode.obj"
if ($LASTEXITCODE -ne 0) { throw 'Unicode dialog fixture compilation failed' }
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdkRoot\Lib\$sdkVersion\um\x86" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/dialog_unicode_native.exe" "$OutputDirectory/dialog_unicode.obj" kernel32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'Unicode dialog fixture linking failed' }
Push-Location $OutputDirectory
try {
    $process = Start-Process -FilePath (Join-Path $OutputDirectory 'dialog_unicode_native.exe') -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Unicode dialog native fixture failed with exit code $($process.ExitCode)" }
} finally { Pop-Location }
$bytes = [System.IO.File]::ReadAllBytes((Join-Path $OutputDirectory 'dialog-unicode-native.bin'))
$words = for ($index = 0; $index -lt $bytes.Length; $index += 4) { [BitConverter]::ToUInt32($bytes, $index) }
$words | ConvertTo-Json -Compress | Set-Content (Join-Path $OutputDirectory 'native.json')
Write-Output ("Native DWORD transcript: " + ($words -join ', '))
Assert-Transcript $words 1812 $false
foreach ($variant in @('data-only', 'dialogs')) {
    $defines = if ($variant -eq 'data-only') { @('/D', 'DATA_ONLY') } else { @() }
    & (Join-Path $llvm 'llvm-rc.exe') @defines '/FO' "$OutputDirectory/$variant.res" (Join-Path $PSScriptRoot 'dialog_unicode_native.rc')
    if ($LASTEXITCODE -ne 0) { throw "Native resource fixture $variant compilation failed" }
    if ($variant -eq 'dialogs') {
        $resourcePath = "$OutputDirectory/$variant.res"
        $resourceBytes = [System.IO.File]::ReadAllBytes($resourcePath)
        $oldName = [System.Text.Encoding]::Unicode.GetBytes('NAME' + [char]0)
        $newName = [System.Text.Encoding]::Unicode.GetBytes([string][char]0x5bfe + [char]0x8a71 + [char]0xd83c + [char]0xdf38 + [char]0)
        $replaced = 0
        for ($offset = 0; $offset -le $resourceBytes.Length - $oldName.Length; ++$offset) {
            $matches = $true
            for ($part = 0; $part -lt $oldName.Length; ++$part) { if ($resourceBytes[$offset + $part] -ne $oldName[$part]) { $matches = $false; break } }
            if ($matches) { [Array]::Copy($newName, 0, $resourceBytes, $offset, $newName.Length); ++$replaced }
        }
        if ($replaced -ne 1) { throw 'Expected exactly one authored UTF-16 resource name to substitute' }
        [System.IO.File]::WriteAllBytes($resourcePath, $resourceBytes)
    }
    $object = "$OutputDirectory/dialog_unicode.obj"
    if ($variant -eq 'dialogs') {
        $object = "$OutputDirectory/dialog_unicode_resources.obj"
        & (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /DRESOURCE_DIALOGS /clang:--target=i686-pc-windows-msvc @includes (Join-Path $PSScriptRoot 'dialog_unicode_native.c') "/Fo$object"
        if ($LASTEXITCODE -ne 0) { throw 'Native resource callback compilation failed' }
    }
    $executable = "$OutputDirectory/dialog_unicode_$variant.exe"
    & (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdkRoot\Lib\$sdkVersion\um\x86" /subsystem:console /entry:mainCRTStartup "/out:$executable" $object "$OutputDirectory/$variant.res" kernel32.lib user32.lib
    if ($LASTEXITCODE -ne 0) { throw "Native resource fixture $variant linking failed" }
    Push-Location $OutputDirectory
    try {
        $process = Start-Process -FilePath $executable -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Native resource fixture $variant failed" }
    } finally { Pop-Location }
    $bytes = [System.IO.File]::ReadAllBytes((Join-Path $OutputDirectory 'dialog-unicode-native.bin'))
    $words = for ($index = 0; $index -lt $bytes.Length; $index += 4) { [BitConverter]::ToUInt32($bytes, $index) }
    $words | ConvertTo-Json -Compress | Set-Content (Join-Path $OutputDirectory "native-$variant.json")
    if ($variant -eq 'data-only') { Assert-Transcript $words 1813 $false }
    else { Assert-Transcript $words 1814 $true }
    Write-Output ("Native $variant DWORD transcript: " + ($words -join ', '))
}
