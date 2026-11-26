param(
    [Parameter(Mandatory)][string]$Fxc,
    [Parameter(Mandatory)][string]$Output
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

    public static void Write(string input, string output) {
        byte[] code = File.ReadAllBytes(input);
        const uint DisableDebugInfo = 0x10;
        const uint InstructionOnly = 0x40;
        IntPtr blob;
        Marshal.ThrowExceptionForHR(D3DDisassemble(code, (UIntPtr)(uint)code.Length,
            DisableDebugInfo | InstructionOnly, IntPtr.Zero, out blob));
        try {
            IntPtr vtable = Marshal.ReadIntPtr(blob);
            var get = Marshal.GetDelegateForFunctionPointer<GetPointer>(Marshal.ReadIntPtr(vtable, 3 * IntPtr.Size));
            string text = Marshal.PtrToStringAnsi(get(blob)).Replace("\r\n", "\n");
            text = System.Text.RegularExpressions.Regex.Replace(text, @"[ \t]+(?=\n|$)", "");
            File.WriteAllText(output, text);
        } finally {
            Marshal.Release(blob);
        }
    }
}
'@

function Compile($Source, $Entry, $Profile, $Name) {
    $define = @()
    if ($Profile -match 'level_9') { $define = @('/D', 'LEVEL9=1') }
    $object = Join-Path $Output "$Name.dxbc"
    & $Fxc /nologo /T $Profile /E $Entry @define /O3 /Qstrip_debug /Qstrip_priv /Fo $object (Join-Path $PSScriptRoot $Source)
    if ($LASTEXITCODE -ne 0) { throw "FXC failed for $Name" }
    [ShaderAssembly]::Write($object, (Join-Path $Output "$Name.asm"))
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
