"""Regenerate the four pinned GPU upscalers with Slang 2026.19.

Usage: python3 tools/upscaling/generate.py /path/to/slangc
Input: vendor/upscaling shader sources. Output: the display upscale stage's
WGSL, in the GPU layer's binding groups, and its model tables.
Temporary compiler inputs stay in target/scratch/upscaling-generator.
"""
import pathlib
import re
import subprocess
import json
import sys
import fuse
BASE = pathlib.Path(__file__).resolve().parents[2]

def generate(compiler):
    sources = BASE / 'vendor/upscaling'
    root = BASE / 'target/scratch/upscaling-generator'
    root.mkdir(parents=True, exist_ok=True)
    common = (sources / 'StubDefs.hlsli').read_text()
    common = common[common.index('#define MF '):]
    common = common[:common.index('uint GetFrameCount')]
    common = common.replace('return uint2(0, 0);', 'return dimensions.xy;', 1).replace('return float2(0, 0);', 'return 1.0 / float2(dimensions.xy);', 1).replace('return float2(0, 0);', 'return dimensions.zw;', 1).replace('return float2(0, 0);', 'return 1.0 / float2(dimensions.zw);', 1).replace('return float2(0, 0);', 'return float2(dimensions.zw) / float2(dimensions.xy);', 1)
    for filename in ['Anime4K_Upscale_Denoise_L', 'CuNNy-4x12-NVL', 'k7_modernAnime_FHD_x2']:
        s = (sources / (filename + '.hlsl')).read_text()
        # Magpie computes MF types in half precision when a model declares FP16.
        precision = re.sub('^(#define MF[0-9x]*) float', '\\1 half', common, flags=re.M) if '//!CAPABILITY FP16' in s else common
        parts = re.split('//!PASS \\d+\\s*\\n', s)
        formats = {}
        for desc in parts[0].split('//!TEXTURE')[1:]:
            name = re.search('Texture2D (\\w+);', desc)[1]
            formats[name] = 'rgba8' if 'R8G8B8A8_UNORM' in desc else 'rgba16f'
        samplers = re.findall('//!FILTER (\\w+)\\s*\\nSamplerState (\\w+);', parts[0])
        shared = '\n'.join((line for line in parts[0].splitlines() if line.startswith('#define ')))
        manifest = []
        outdir = root / filename
        outdir.mkdir(exist_ok=True)
        for (n, body) in enumerate(parts[1:], 1):
            ins = re.search('//!IN ([^\\n]+)', body)[1].replace(' ', '').split(',')
            outs = re.search('//!OUT ([^\\n]+)', body)[1].replace(' ', '').split(',')
            block = int(re.search('//!BLOCK_SIZE (\\d+)', body)[1])
            pre = '[[vk::binding(0,0)]] cbuffer Params { uint4 dimensions; };\n' + precision + '\n' + shared + '\n'
            binding = 1
            for name in ins:
                pre += f'[[vk::binding({binding},0)]] Texture2D<float4> {name};\n'
                binding += 1
            for name in outs:
                pre += f'[[vk::binding({binding},0)]] [[vk::image_format("{formats[name]}")]] WTexture2D<float4> {name};\n'
                binding += 1
            for (filt, name) in samplers:
                pre += f'[[vk::binding({binding},0)]] SamplerState {name};\n'
                binding += 1
            for name in outs:
                body = re.sub('\\b' + name + '\\[([^\\]]+)\\]\\s*=\\s*([^;]+);', lambda m: name + '.Store(' + m[1] + ', ' + m[2] + ');', body)
            src = pre + body + f'\n[numthreads(64,1,1)] void main(uint3 group : SV_GroupID, uint3 tid : SV_GroupThreadID) {{ Pass{n}(group.xy * {block}, tid); }}\n'
            p = outdir / f'{n}.slang'
            p.write_text(src)
            # Upstream relies on implicit float-to-half conversions.
            r = subprocess.run([str(compiler), str(p), '-entry', 'main', '-stage', 'compute', '-target', 'wgsl', '-warnings-disable', '30081', '-o', str(outdir / f'{n}.wgsl')], capture_output=True, text=True)
            if r.returncode:
                print(filename, n, r.stderr[:3500])
                raise SystemExit(1)
            manifest.append(dict(inputs=ins, outputs=outs, block=block, samplers=samplers, formats=[formats[o] for o in outs]))
        (outdir / 'passes.json').write_text(json.dumps(manifest, indent=2))
        print(filename, 'compiled', len(manifest), flush=True)
    s = (sources / 'ArtCNN_C4F16.glsl').read_text()
    parts = s.split('//!DESC ')[1:]
    outdir = root / 'ArtCNN_C4F16'
    outdir.mkdir(exist_ok=True)
    manifest = []
    for (n, body) in enumerate(parts, 1):
        ins = re.findall('//!BIND (\\w+)', body)
        save = re.search('//!SAVE (\\w+)', body)
        last = not save
        outs = [save[1] if save else 'OUTPUT']
        if last:
            ins.append('INPUT')
        pre = '[[vk::binding(0,0)]] cbuffer Params { uint4 dimensions; };\n'
        for (b, name) in enumerate(ins, 1):
            pre += f'[[vk::binding({b},0)]] Texture2D<float4> {name};\n'
        b = len(ins) + 1
        pre += f'[[vk::binding({b},0)]] [[vk::image_format("rgba16f")]] WTexture2D<float4> {outs[0]};\n'
        pre += f'[[vk::binding({b + 1},0)]] SamplerState sam;\n'
        for name in ins:
            pre += f'float4 {name}_tex(float2 uv) {{ return {name}.SampleLevel(sam, uv, 0); }}\n'
        body = body[body.index('vec4 hook()'):]
        body = re.sub('mat4\\(([^)]+)\\) \\* ([^;]+);', 'mul(\\2, float4x4(\\1));', body)
        body = body.replace('vec4', 'float4').replace('ivec2', 'int2').replace('vec2', 'float2').replace('fract(', 'frac(')
        body = body.replace('float4 hook()', 'float4 hook(float2 pos)')
        for name in ins:
            body = re.sub(name + '_texOff\\((float2\\([^)]*\\))\\)', name + '_tex(pos + \\1 / float2(dimensions.xy))', body)
            body = body.replace(name + '_pos', 'pos').replace(name + '_size', 'float2(dimensions.xy)').replace(name + '_pt', '(1.0 / float2(dimensions.xy))')
        if 'LUMA' in ins:
            pre = pre.replace('return LUMA.SampleLevel(sam, uv, 0);', 'return float4(dot(LUMA.SampleLevel(sam, uv, 0).rgb, float3(0.2126, 0.7152, 0.0722)), 0, 0, 1);')
        size = 'dimensions.zw' if last else 'dimensions.xy'
        main = f'\n[numthreads(8,8,1)] void main(uint3 group : SV_GroupID, uint3 tid : SV_GroupThreadID) {{ uint2 id = group.xy * 8 + tid.xy; if(any(id >= {size})) return; float2 pos = (float2(id) + 0.5) / float2({size}); float4 v=hook(pos);'
        if last:
            main += 'float3 rgb=INPUT.SampleLevel(sam,pos,0).rgb; v=float4(rgb + (v.x - dot(rgb,float3(0.2126,0.7152,0.0722))),1);'
        main += f'{outs[0]}.Store(id,v); }}'
        p = outdir / f'{n}.slang'
        p.write_text(pre + body + main)
        r = subprocess.run([str(compiler), str(p), '-entry', 'main', '-stage', 'compute', '-target', 'wgsl', '-o', str(outdir / f'{n}.wgsl')], capture_output=True, text=True)
        if r.returncode:
            print(n, r.stderr[:2500])
            raise SystemExit(1)
        manifest.append(dict(inputs=['INPUT' if x == 'LUMA' else x for x in ins], outputs=outs, block=8, samplers=[['LINEAR', 'sam']], formats=['rgba16f']))
    (outdir / 'passes.json').write_text(json.dumps(manifest, indent=2))
    print('ArtCNN compiled', len(manifest))
    emit(root)

UPSCALE = BASE / 'crates/adv-display/src/stages/upscale'
MODELS = [('Anime4k', 'Anime4K_Upscale_Denoise_L'), ('Cunny', 'CuNNy-4x12-NVL'), ('Artcnn', 'ArtCNN_C4F16'), ('K7', 'k7_modernAnime_FHD_x2')]
HEADER = '// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.\n'
# Binding groups of the GPU layer: uniforms, sampled images, samplers, storage.
GROUPS = [('var<uniform>', 0), ('texture_2d<', 1), ('sampler', 2), ('texture_storage_2d<', 3)]


def rebind(source):
    """Move each binding to its kind's group; the final layer writes the 8-bit
    presentable buffer. Binding numbers, and so the kernels, stay unchanged."""
    def group(m):
        decl = m[0].replace('@group(0)', f'@group({next(g for marker, g in GROUPS if marker in m[0])})')
        return decl.replace('rgba16float', 'rgba8unorm') if m[2].startswith('OUTPUT_') else decl
    return fuse.DECL.sub(group, source)


# Each workgroup covers one listed cell of its pass's output grid: workgroup
# `i` reads `cells[i]`, packed as x | y << 16 in units of the pass's block.
CELLS = '''@group(3) @binding(0) var<storage, read> cells: array<u32>;
fn cell(index: u32) -> vec2<u32> {
    let packed = cells[index];
    return vec2<u32>(packed & 0xffffu, packed >> 16u);
}
'''


def listed(source):
    """Take each workgroup's block from the cell list instead of its id."""
    group = re.search(r'@builtin\(workgroup_id\)\s*(\w+)', source)[1]
    source, count = re.subn(r'\b' + group + r'\.xy\b', f'cell({group}.x)', source)
    assert count == 1, 'one block origin per entry'
    entry = source.index('@compute')
    return source[:entry] + CELLS + source[entry:]


def inline(source):
    """Expand every MulAdd helper at its call sites: Adreno 6xx compilers crash
    on functions that take a matrix. The products and sums stay in order."""
    for name, x, y, a, body in re.findall(r'fn (MulAdd_\w+)\( *(\w+) *:[^,]+, *(\w+) *:[^,]+, *(\w+) *:[^)]+\)[^{]*\{\s*return ([^;]+);\s*\}\n*', source):
        assert body == f'((({y}) * ({x}))) + {a}', body
        source = re.sub(r'fn ' + name + r'\([^{]*\{[^}]*\}\n*', '', source)
        expand = lambda args: f'((({args[1]}) * ({fuse.calls(args[0], name, expand)}))) + ({fuse.calls(args[2], name, expand)})'
        source = fuse.calls(source, name, expand)
    assert 'MulAdd' not in source
    return source


def write(shader, source):
    (UPSCALE / shader).parent.mkdir(parents=True, exist_ok=True)
    (UPSCALE / shader).write_text(HEADER + listed(rebind(inline(source))).rstrip() + '\n')


def slots(passes):
    """Reuse a tensor only after its last reader. The output is the presentable
    buffer, not a tensor."""
    last = {}
    formats = {}
    for (i, p) in enumerate(passes):
        for name in p['inputs'] + p['outputs']:
            last[name] = i
        formats.update(zip(p['outputs'], p['formats']))
    names = {}
    allocated = []
    for (i, p) in enumerate(passes):
        for name in p['outputs']:
            if name in names or name == 'OUTPUT':
                continue
            slot = next((j for (j, (fmt, end)) in enumerate(allocated) if fmt == formats[name] and end < i), len(allocated))
            if slot == len(allocated):
                allocated.append((formats[name], last[name]))
            else:
                allocated[slot] = (formats[name], last[name])
            names[name] = slot
    return (names, [fmt for (fmt, _) in allocated])


def model(passes):
    """One model table entry; `passes` carry their emitted shader paths."""
    (names, tensors) = slots(passes)
    rust = ['Model { tensors: &[' + ', '.join('Rgba8Unorm' if fmt == 'rgba8' else 'Rgba16Float' for fmt in tensors) + '], passes: &[']
    for p in passes:
        roles = ['Uniform']
        cells = '(0, Cells), '
        roles += ['Source' if n == 'INPUT' else f'Tensor({names[n]})' for n in p['inputs']]
        roles += ['Output' if n == 'OUTPUT' else f'Store({names[n]})' for n in p['outputs']]
        roles += ['Linear' if f == 'LINEAR' else 'Nearest' for (f, _) in p['samplers']]
        used = {int(n) for n in re.findall('@binding\\((\\d+)\\)', (UPSCALE / p['shader']).read_text())}
        entries = ', '.join(f'({n}, {role})' for (n, role) in enumerate(roles) if n in used)
        scale = 2 if 'OUTPUT' in p['outputs'] else 1
        rust.append(f"Pass {{ shader: \"{p['shader']}\", bindings: &[{cells}{entries}], block: {p['block']}, scale: {scale}, radius: {p['radius']} }},")
    rust.append('] }')
    return '\n'.join(rust)


def emit(root):
    tables = {'REFERENCE': [], 'FUSED': [], 'MATRIX': []}
    for (variant, name) in MODELS:
        passes = json.loads((root / name / 'passes.json').read_text())
        for (i, p) in enumerate(passes, 1):
            p.update(shader=f'{name}/{i}.wgsl', radius=1)
            write(p['shader'], (root / name / f'{i}.wgsl').read_text())
        tables['REFERENCE'].append(model(passes))
        fused = fuse.combine(root, name, passes, write)
        tables['FUSED'].append(model(fused))
        matrix = fuse.combine(root, name, passes, write, matrix=True)
        index = len(tables['FUSED']) - 1
        tables['MATRIX'].append(model(matrix) if matrix else f'FUSED[{index}]')
    rust = ['// Generated by tools/upscaling/generate.py; see vendor/upscaling/NOTICE.',
            '//! Model tables in `Upscaler` order (Anime4K, CuNNy, ArtCNN, k7).',
            'use super::table::{Format::*, Model, Pass, Role::*};']
    docs = {'REFERENCE': 'Unfused upstream passes, the reference for tests.',
            'FUSED': 'Adjacent layers fused through workgroup storage.',
            'MATRIX': 'Fused, with half-precision 8x8 matrix input convolutions where the model allows.'}
    for (table, entries) in tables.items():
        rust.append(f'/// {docs[table]}')
        rust.append(f'pub const {table}: [Model; 4] = [' + ',\n'.join(entries) + '];')
    (UPSCALE / 'models.rs').write_text('\n'.join(rust) + '\n')
    subprocess.run(['rustfmt', '--edition', '2024', str(UPSCALE / 'models.rs')], check=True)


if __name__ == '__main__':
    generate(pathlib.Path(sys.argv[1]).resolve())
