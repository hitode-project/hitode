<#
.SYNOPSIS
Record authored raw x86 INT 2E illegal-instruction exceptions on a WOW64 host.
.DESCRIPTION
OutputDirectory stores authored binaries, 64 scalar outcomes and cmd.txt under
target/investigations. Requires a 64-bit Windows OS running the authored x86 PE
under WOW64. This is a native-only host reference, not an oracle for the guest's
native x86 OS profile. NativeOnly is accepted for existing invocations.
Checks illegal-instruction SEH, prefixed/unprefixed CD opcode PCs, registers/flags/stack and
LastError preservation. No original samples or native DLLs are run.
.EXAMPLE
pwsh tools/fixtures/test-invalid-system-service.ps1 -NativeOnly
#>
param(
    [string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/investigations/invalid-system-service'),
    [switch]$NativeOnly
)
$ErrorActionPreference='Stop'
if(-not $IsWindows -or -not [Environment]::Is64BitOperatingSystem){
    throw 'this native INT 2E oracle requires WOW64 on 64-bit Windows'
}
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $OutputDirectory.Contains([IO.Path]::DirectorySeparatorChar+'target'+[IO.Path]::DirectorySeparatorChar+'investigations'+[IO.Path]::DirectorySeparatorChar)){
    throw 'system service evidence must stay inside target/investigations'
}
$nativeDirectory=Join-Path $OutputDirectory 'native'
New-Item -ItemType Directory -Force -Path $nativeDirectory | Out-Null
$llvm='C:\Program Files\LLVM\bin'
$sdk='C:\Program Files (x86)\Windows Kits\10'
$version='10.0.26100.0'
$source=Join-Path $PSScriptRoot 'invalid-system-service.c'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" $source "/Fo$nativeDirectory/fixture.obj"
if($LASTEXITCODE-ne 0){throw 'invalid system service fixture compile failed'}
$fixture=Join-Path $nativeDirectory 'fixture.exe'
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$fixture" "$nativeDirectory/fixture.obj" kernel32.lib
if($LASTEXITCODE-ne 0){throw 'invalid system service fixture link failed'}
@(
    'command: fixture.exe (authored invalid INT 2E selectors; ten-second timeout)',
    "environment: native Windows; 64-bit OS=$([Environment]::Is64BitOperatingSystem); no sample input",
    "source_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash)",
    "fixture_sha256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $fixture).Hash)"
) | Set-Content -LiteralPath (Join-Path $nativeDirectory 'cmd.txt')
$native=Start-Process -FilePath $fixture -WorkingDirectory $nativeDirectory -WindowStyle Hidden -PassThru
try{
    if(-not $native.WaitForExit(10000)){throw 'native invalid system service fixture exceeded ten seconds'}
    if($native.ExitCode-ne 0){throw "native invalid system service fixture failed: $($native.ExitCode)"}
}finally{
    try{if(-not $native.HasExited){$native.Kill();$native.WaitForExit(1000)|Out-Null}}
    finally{$native.Dispose()}
}
$reference=[IO.File]::ReadAllBytes((Join-Path $nativeDirectory 'invalid-system-service.bin'))
if($reference.Length-ne 64*4){throw 'native invalid system service results missing'}
for($case=0;$case-lt 4;$case++){
    $values=@(for($i=0;$i-lt 16;$i++){[BitConverter]::ToUInt32($reference,($case*16+$i)*4)})
    $selector=if($case%2-eq 0){[uint32]::MaxValue}else{[uint32]0xfff}
    $offset=if($case-ge 2){1}else{0}
    $expected=@($selector,0,$selector,[uint32]3221225501,0,0,$offset,$offset,1,1,1,1,1,1,1,1)
    for($i=0;$i-lt 16;$i++){
        if($values[$i]-ne $expected[$i]){throw "native WOW64 INT 2E invariant failed at DWORD $($case*16+$i)"}
    }
    Write-Output ("native case {0}: service=0x{1:x8} returned={2} eax=0x{3:x8} exception=0x{4:x8} count={5} record/context offsets={6}/{7}; flags/esp/ebx/ecx/edx/esi-edi/LastError={8}" -f $case,$values[0],$values[1],$values[2],$values[3],$values[8],$values[6],$values[7],($values[9..15] -join ','))
}
