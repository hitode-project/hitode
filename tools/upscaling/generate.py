"""Regenerate the four pinned GPU upscalers with Slang 2026.19.

Usage: python3 tools/upscaling/generate.py /path/to/slangc
Input: vendor/upscaling shader sources. Output: shell WGSL and model tables.
Temporary compiler inputs stay in target/scratch/upscaling-generator.
"""
import pathlib
import re
import subprocess
import json
import sys
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
            pre = '[[vk::binding(0,0)]] cbuffer Params { uint4 dimensions; uint4 origin; };\n' + common + '\n' + shared + '\n'
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
            # origin.xy is the damaged region's corner in input pixels, origin.zw in output pixels.
            origin = 'origin.zw' if 'OUTPUT' in outs else 'origin.xy'
            src = pre + body + f'\n[numthreads(64,1,1)] void main(uint3 group : SV_GroupID, uint3 tid : SV_GroupThreadID) {{ Pass{n}(group.xy * {block} + {origin}, tid); }}\n'
            p = outdir / f'{n}.slang'
            p.write_text(src)
            r = subprocess.run([str(compiler), str(p), '-entry', 'main', '-stage', 'compute', '-target', 'wgsl', '-o', str(outdir / f'{n}.wgsl')], capture_output=True, text=True)
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
        pre = '[[vk::binding(0,0)]] cbuffer Params { uint4 dimensions; uint4 origin; };\n'
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
        origin = 'origin.zw' if last else 'origin.xy'
        main = f'\n[numthreads(8,8,1)] void main(uint3 group : SV_DispatchThreadID) {{ uint2 id = group.xy + {origin}; if(any(id >= {size})) return; float2 pos = (float2(id) + 0.5) / float2({size}); float4 v=hook(pos);'
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

def slots(passes):
    """Reuse a texture only after its last reader, retaining the final output."""
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
            if name in names:
                continue
            kind = (formats[name], 2 if name == 'OUTPUT' else 1)
            slot = next((j for (j, (fmt, scale, end)) in enumerate(allocated) if (fmt, scale) == kind and end < i), len(allocated))
            if slot == len(allocated):
                allocated.append((*kind, last[name]))
            else:
                allocated[slot] = (*kind, last[name])
            names[name] = slot
    return (names, [(fmt, scale) for (fmt, scale, _) in allocated])

def emit(root):
    destination = BASE / 'crates/adv-shell/src/upscaling'
    models = [('Anime4k', 'Anime4K_Upscale_Denoise_L'), ('Cunny', 'CuNNy-4x12-NVL'), ('Artcnn', 'ArtCNN_C4F16'), ('K7', 'k7_modernAnime_FHD_x2')]
    rust = ['// Generated by tools/upscaling/generate.py; see vendor/upscaling/NOTICE.', 'use super::{Binding::*, Model, Pass, Upscaler};', 'use wgpu::TextureFormat::{Rgba8Unorm, Rgba16Float};', 'pub(super) fn model(value: Upscaler) -> Model {', 'match value {', 'Upscaler::Off => unreachable!(),']
    for (variant, name) in models:
        passes = json.loads((root / name / 'passes.json').read_text())
        (names, textures) = slots(passes)
        rust.append(f'Upscaler::{variant} => Model {{ textures: &[')
        for (fmt, scale) in textures:
            rust.append(f"({('Rgba8Unorm' if fmt == 'rgba8' else 'Rgba16Float')}, {scale}),")
        rust.append(f"], output: {names['OUTPUT']}, passes: &[")
        directory = destination / name
        directory.mkdir(exist_ok=True)
        for (i, p) in enumerate(passes, 1):
            wgsl = (root / name / f'{i}.wgsl').read_text()
            (directory / f'{i}.wgsl').write_text('// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.\n' + wgsl.rstrip() + '\n')
            bindings = ['Uniform']
            bindings += ['Source' if n == 'INPUT' else f'Texture({names[n]})' for n in p['inputs']]
            bindings += [f'Texture({names[n]})' for n in p['outputs']]
            bindings += ['Linear' if f == 'LINEAR' else 'Nearest' for (f, _) in p['samplers']]
            used = {int(n) for n in re.findall('@binding\\((\\d+)\\)', wgsl)}
            entries = ','.join((f'({n},{value})' for (n, value) in enumerate(bindings) if n in used))
            rust.append(f'''Pass {{ source: include_str!("{name}/{i}.wgsl"), bindings: &[{entries}], block: {p['block']}, scale: {(2 if 'OUTPUT' in p['outputs'] else 1)} }},''')
        rust.append('] },')
    rust.append('}\n}')
    (destination / 'models.rs').write_text('\n'.join(rust) + '\n')
    subprocess.run(['rustfmt', '--edition', '2024', str(destination / 'models.rs')], check=True)
if __name__ == '__main__':
    generate(pathlib.Path(sys.argv[1]).resolve())
