param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/fixtures/d3d9-shader-models'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
if(-not ('AdvShaderAssembler' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class AdvShaderAssembler {
    [DllImport("d3dcompiler_47.dll",CallingConvention=CallingConvention.StdCall)]
    static extern int D3DAssemble(byte[] source,UIntPtr size,IntPtr name,IntPtr defines,IntPtr include,uint flags,out IntPtr code,out IntPtr error);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] delegate IntPtr Pointer(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] delegate UIntPtr Size(IntPtr self);
    static byte[] Read(IntPtr blob) {
        IntPtr vtable=Marshal.ReadIntPtr(blob);
        Pointer pointer=(Pointer)Marshal.GetDelegateForFunctionPointer(Marshal.ReadIntPtr(vtable,3*IntPtr.Size),typeof(Pointer));
        Size size=(Size)Marshal.GetDelegateForFunctionPointer(Marshal.ReadIntPtr(vtable,4*IntPtr.Size),typeof(Size));
        byte[] bytes=new byte[checked((int)size(blob).ToUInt64())];Marshal.Copy(pointer(blob),bytes,0,bytes.Length);return bytes;
    }
    public static byte[] Assemble(string text) {
        byte[] source=Encoding.ASCII.GetBytes(text);IntPtr code,error;
        int hr=D3DAssemble(source,(UIntPtr)source.Length,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,0,out code,out error);
        try {if(hr<0)throw new Exception("D3DAssemble 0x"+hr.ToString("x8")+": "+(error==IntPtr.Zero?"":Encoding.ASCII.GetString(Read(error))));return Read(code);}
        finally {if(error!=IntPtr.Zero)Marshal.Release(error);if(code!=IntPtr.Zero)Marshal.Release(code);}
    }
}
'@
}
$asm=Get-Content -Raw (Join-Path $PSScriptRoot 'd3d9-shader-models.asm')
$matches=[regex]::Matches($asm,'(?ms)^// CASE ([a-zA-Z0-9_]+)\r?\n(.*?)(?=^// CASE |\z)')
$header=Join-Path $OutputDirectory 'd3d9-shader-models-generated.h'
Set-Content -LiteralPath $header -Value '// Self-authored assembly; generated artifacts stay in target.' -Encoding ascii
foreach($entry in $matches) {
    $name=$entry.Groups[1].Value
    $bytes=[AdvShaderAssembler]::Assemble($entry.Groups[2].Value)
    [IO.File]::WriteAllBytes((Join-Path $OutputDirectory "$name.bin"),$bytes)
    $words=@(for($i=0;$i-lt $bytes.Length;$i+=4){'0x{0:x8}u' -f [BitConverter]::ToUInt32($bytes,$i)})
    Add-Content -LiteralPath $header -Value "static const DWORD $name[] = {$($words -join ',')};" -Encoding ascii
    Write-Output "$name`: $($bytes.Length / 4) authored tokens"
}
$llvm='C:\Program Files\LLVM\bin';$sdk='C:\Program Files (x86)\Windows Kits\10';$version='10.0.26100.0'
& (Join-Path $llvm 'clang-cl.exe') /nologo /c /O2 /GS- /Gs9999999 /Zl /clang:-fno-builtin /clang:--target=i686-pc-windows-msvc "/I$sdk/Include/$version/um" "/I$sdk/Include/$version/shared" "/I$sdk/Include/$version/ucrt" "/I$OutputDirectory" (Join-Path $PSScriptRoot 'd3d9-shader-models.c') "/Fo$OutputDirectory/d3d9-shader-models.obj"
if($LASTEXITCODE-ne 0){throw 'shader model fixture compile failed'}
& (Join-Path $llvm 'lld-link.exe') /nologo /machine:x86 /nodefaultlib /dynamicbase /base:0x400000 /safeseh:no /timestamp:0 "/libpath:$sdk/Lib/$version/um/x86" /subsystem:windows /entry:mainCRTStartup "/out:$OutputDirectory/fixture_d3d9-shader-models.exe" "$OutputDirectory/d3d9-shader-models.obj" kernel32.lib user32.lib d3d9.lib
if($LASTEXITCODE-ne 0){throw 'shader model fixture link failed'}
