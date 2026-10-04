param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/architecture-acceptance/file-mutation-native'))
$ErrorActionPreference='Stop'
$mutationRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/architecture-acceptance'))
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(!$OutputDirectory.StartsWith($mutationRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Fixture output must stay under target/architecture-acceptance'}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$mutationOutput=Join-Path $OutputDirectory 'file-mutation.bin'
if(Test-Path -LiteralPath $mutationOutput){Remove-Item -LiteralPath $mutationOutput}
$mutationSdk='C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x86'
& 'C:/Program Files/LLVM/bin/clang-cl.exe' /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$PSScriptRoot" (Join-Path $PSScriptRoot 'file-mutation.c') "/Fo$OutputDirectory/file-mutation.obj"
if($LASTEXITCODE -ne 0){throw 'Own file mutation fixture compile failed'}
& 'C:/Program Files/LLVM/bin/lld-link.exe' /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$mutationSdk" /subsystem:console /entry:mainCRTStartup "/out:$OutputDirectory/fixture_file_mutation.exe" "$OutputDirectory/file-mutation.obj" kernel32.lib
if($LASTEXITCODE -ne 0){throw 'Own file mutation fixture link failed'}
$mutationChild=Start-Process -FilePath (Join-Path $OutputDirectory 'fixture_file_mutation.exe') -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru
try{
    if(!$mutationChild.WaitForExit(5000)){throw 'Own file mutation child timed out'}
    if($mutationChild.ExitCode -ne 0){throw "Own file mutation child failed: $($mutationChild.ExitCode)"}
}finally{
    if(!$mutationChild.HasExited){$mutationChild.Kill();if(!$mutationChild.WaitForExit(1000)){Write-Warning 'Owned child cleanup exceeded one second'}}
    $mutationChild.Dispose()
}
$mutationBytes=[IO.File]::ReadAllBytes($mutationOutput)
if($mutationBytes.Length -ne 170*4){throw "Unexpected oracle length $($mutationBytes.Length)"}
for($mutationCase=0;$mutationCase -lt 14;$mutationCase++){
    $mutationWords=0..11 | ForEach-Object{[BitConverter]::ToUInt32($mutationBytes,($mutationCase*12+$_)*4)}
    if($mutationCase -lt 7){
        if($mutationWords[1] -ne 0 -or $mutationWords[2] -ne 1224 -or $mutationWords[5] -ne 4096 -or $mutationWords[6] -ne 0x11111111 -or $mutationWords[10] -ne 4096 -or $mutationWords[11] -ne 0x11111111){throw "Mapped CopyFile contract changed at case $mutationCase"}
    }else{
        $mutationVariant=if($mutationCase -eq 13){1}else{($mutationCase-7)%3}
        $mutationSuccess=if($mutationVariant -eq 0){0}else{1}
        $mutationLength=if($mutationVariant -eq 2){19}else{12}
        # Native writes the short prefix, then fails truncation with 1224.
        $mutationKey=if($mutationVariant -eq 0){0x0d0a0d78}elseif($mutationVariant -eq 1){0x0d77656e}else{0x676e6f6c}
        if($mutationWords[1] -ne $mutationSuccess -or ($mutationSuccess -eq 0 -and $mutationWords[2] -ne 1224) -or $mutationWords[5] -ne $mutationLength -or $mutationWords[6] -ne $mutationKey -or $mutationWords[10] -ne $mutationLength -or $mutationWords[11] -ne $mutationKey -or ($mutationCase -ne 13 -and $mutationWords[7] -ne $mutationKey)){throw "Mapped Profile contract changed at case $mutationCase"}
    }
    "case $mutationCase $($mutationWords -join ' ')"
}
if([BitConverter]::ToUInt32($mutationBytes,168*4) -ne 0 -or [BitConverter]::ToUInt32($mutationBytes,169*4) -ne 32){throw 'Same-source native contract changed'}
"same-source $([BitConverter]::ToUInt32($mutationBytes,168*4)) $([BitConverter]::ToUInt32($mutationBytes,169*4))"
