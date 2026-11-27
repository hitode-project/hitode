param(
    [Parameter(Mandatory)][string]$Fxc,
    [Parameter(Mandatory)][string]$Output,
    [string]$Encoded
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Output | Out-Null

Add-Type @'
using System;
using System.IO;
using System.Runtime.InteropServices;

public static class ShaderAssembly {
    [DllImport("d3dcompiler_47.dll", ExactSpelling = true)]
    static extern int D3DDisassemble(byte[] code, UIntPtr size, uint flags, IntPtr comments, out IntPtr blob);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate IntPtr GetPointer(IntPtr blob);

    [DllImport("d3d11.dll", ExactSpelling = true)]
    static extern int D3D11CreateDevice(IntPtr adapter, uint driver, IntPtr software, uint flags,
        IntPtr levels, uint levelCount, uint sdk, out IntPtr device, IntPtr level, IntPtr context);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate int CreateShader(IntPtr device, byte[] code, UIntPtr size, IntPtr linkage, out IntPtr shader);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate int CreateClassLinkage(IntPtr device, out IntPtr linkage);

    static T Method<T>(IntPtr obj, int slot) where T : Delegate {
        return Marshal.GetDelegateForFunctionPointer<T>(
            Marshal.ReadIntPtr(Marshal.ReadIntPtr(obj), slot * IntPtr.Size));
    }

    public static int Create(string input, uint driver, bool dynamicLinkage) {
        byte[] code = File.ReadAllBytes(input);
        int[] shaderSlots = {15, 12, 13, 16, 17, 18};
        int stage = -1;
        for (int i = 0; i < BitConverter.ToInt32(code, 28); ++i) {
            int offset = BitConverter.ToInt32(code, 32 + 4 * i);
            string tag = System.Text.Encoding.ASCII.GetString(code, offset, 4);
            if (tag == "SHDR" || tag == "SHEX")
                stage = BitConverter.ToUInt16(code, offset + 10);
        }
        IntPtr device;
        Marshal.ThrowExceptionForHR(D3D11CreateDevice(IntPtr.Zero, driver, IntPtr.Zero, 0,
            IntPtr.Zero, 0, 7, out device, IntPtr.Zero, IntPtr.Zero));
        IntPtr linkage = IntPtr.Zero;
        try {
            if (dynamicLinkage) {
                Marshal.ThrowExceptionForHR(Method<CreateClassLinkage>(device, 19)(device, out linkage));
            }
            IntPtr shader;
            int hr = Method<CreateShader>(device, shaderSlots[stage])(device, code,
                (UIntPtr)(uint)code.Length, linkage, out shader);
            if (shader != IntPtr.Zero) Marshal.Release(shader);
            return hr;
        } finally {
            if (linkage != IntPtr.Zero) Marshal.Release(linkage);
            Marshal.Release(device);
        }
    }

    public static int Write(string input, string output) {
        byte[] code = File.ReadAllBytes(input);
        const uint DisableDebugInfo = 0x10;
        const uint InstructionOnly = 0x40;
        IntPtr blob;
        int hr = D3DDisassemble(code, (UIntPtr)(uint)code.Length,
            DisableDebugInfo | InstructionOnly, IntPtr.Zero, out blob);
        if (hr < 0) return hr;
        try {
            IntPtr vtable = Marshal.ReadIntPtr(blob);
            var get = Marshal.GetDelegateForFunctionPointer<GetPointer>(Marshal.ReadIntPtr(vtable, 3 * IntPtr.Size));
            string text = Marshal.PtrToStringAnsi(get(blob)).Replace("\r\n", "\n");
            text = System.Text.RegularExpressions.Regex.Replace(text, @"[ \t]+(?=\n|$)", "");
            File.WriteAllText(output, text);
        } finally {
            Marshal.Release(blob);
        }
        return hr;
    }
}
'@

function Compile($Source, $Entry, $Profile, $Name) {
    $define = @()
    if ($Profile -match 'level_9') { $define = @('/D', 'LEVEL9=1') }
    $object = Join-Path $Output "$Name.dxbc"
    & $Fxc /nologo /T $Profile /E $Entry @define /O3 /Qstrip_debug /Qstrip_priv /Fo $object (Join-Path $PSScriptRoot $Source)
    if ($LASTEXITCODE -ne 0) { throw "FXC failed for $Name" }
    $hr = [ShaderAssembly]::Write($object, (Join-Path $Output "$Name.asm"))
    if ($hr -ne 0) { throw "D3DDisassemble failed for $Name with $hr" }
}

foreach ($model in @('4_0', '4_1', '5_0')) {
    Compile dxbc.hlsl vs "vs_$model" "vs_$model"
    Compile dxbc.hlsl indexed_ps "ps_$model" "ps_$model"
    Compile dxbc.hlsl gs "gs_$model" "gs_$model"
    Compile dxbc.hlsl cs "cs_$model" "cs_$model"
}
Compile dxbc.hlsl hs hs_5_0 hs_5_0
Compile dxbc.hlsl ds ds_5_0 ds_5_0
foreach ($level in @('9_1', '9_3')) {
    Compile dxbc.hlsl vs "vs_4_0_level_$level" "vs_4_0_level_$level"
    Compile dxbc.hlsl ps "ps_4_0_level_$level" "ps_4_0_level_$level"
}

foreach ($entry in @(
    'arithmetic', 'control', 'typed_uav', 'doubles', 'atomics', 'raw',
    'buffer_views', 'packed_buffer', 'counters', 'texture_atomic', 'texture_atomic_return',
    'integer_ops', 'atomic_modes', 'float_ops', 'bit_ops', 'branches'
)) {
    Compile shader-core.hlsl $entry cs_5_0 $entry
}
foreach ($entry in @(
    'color', 'discard_pixel', 'texture_ops', 'raster_queries', 'raster_samples', 'dynamic_gather',
    'dynamic_gather_compare', 'level_query', 'derivative_queries', 'multisample_queries',
    'sample_modes', 'coverage', 'early_depth', 'indexed_inputs',
    'residency', 'linkage'
)) {
    Compile shader-core.hlsl $entry ps_5_0 $entry
}
Compile shader-core.hlsl vertex_sample vs_5_0 vertex_sample
Compile shader-core.hlsl derivatives_4_0 ps_4_0 derivatives_4_0
Compile shader-core.hlsl swap_values cs_5_0 swap_values
Compile shader-core.hlsl diagnostic_pixel ps_5_0 diagnostic_pixel
Compile shader-core.hlsl min_precision cs_5_0 min_precision
Compile shader-core.hlsl instanced_geometry gs_5_0 instanced_geometry
Compile dxbc.hlsl limited_hull hs_5_0 limited_hull
Compile d3d11-stages.hlsl stage_members ps_5_0 stage_members
Compile d3d11-stages.hlsl stage_wide_hull hs_5_0 stage_wide_hull
Compile d3d11-stages.hlsl stage_wide_geometry gs_5_0 stage_wide_geometry

if ($Encoded) {
    $rows = @("fixture`thardware_create`twarp_create`tdisassemble")
    $cases = @(Get-ChildItem $Encoded -Filter '*.dxbc' | Sort-Object Name)
    foreach ($name in @('diagnostic_pixel', 'doubles', 'instanced_geometry', 'limited_hull',
                        'linkage', 'min_precision', 'residency', 'swap_values')) {
        $cases += Get-Item (Join-Path $Output "$name.dxbc")
    }
    foreach ($file in $cases) {
        $name = $file.BaseName
        $object = $file.FullName
        if ($name.StartsWith('encoded_')) {
            Copy-Item $object $Output
        }
        $hardware = [ShaderAssembly]::Create($object, 1, $name -eq 'linkage')
        $warp = [ShaderAssembly]::Create($object, 5, $name -eq 'linkage')
        $disassembly = [ShaderAssembly]::Write($object, (Join-Path $Output "$name.asm"))
        $rows += "{0}`t{1:x8}`t{2:x8}`t{3:x8}" -f $name, $hardware, $warp, $disassembly
    }
    [IO.File]::WriteAllText((Join-Path $Output 'creation.tsv'), ($rows -join "`n") + "`n")
}
