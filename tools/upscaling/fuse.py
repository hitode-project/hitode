"""Fuse adjacent pinned convolutions through quantized workgroup storage.

Used by generate.py after Slang compilation. Inputs are WGSL and pass metadata;
outputs are fused WGSL and an SSA texture graph for the same model.
"""
import re

DECL = re.compile(r'@binding\((\d+)\)\s*@group\(0\)\s*var(?:<uniform>)?\s+(\w+)\s*:\s*([^;]+);')


def arguments(source, start):
    """Return a call's balanced argument list and its exclusive end offset."""
    depth, begin, args = 1, start, []
    for at in range(start, len(source)):
        c = source[at]
        depth += (c == '(') - (c == ')')
        if c == ',' and depth == 1:
            args.append(source[begin:at].strip())
            begin = at + 1
        if depth == 0:
            args.append(source[begin:at].strip())
            return args, at + 1
    raise ValueError('unbalanced generated call')


def calls(source, name, rewrite):
    output, at = [], 0
    while (begin := source.find(name + '(', at)) >= 0:
        args, end = arguments(source, begin + len(name) + 1)
        output += [source[at:begin], rewrite(args)]
        at = end
    return ''.join(output) + source[at:]


def rename(source, suffix):
    return re.sub(r'\b(\w+)_(\d+)\b', rf'\1_{suffix}\2', source)


def stage(source, suffix):
    """Turn one generated entry point into a single-pixel convolution call."""
    source, entry = source.split('@compute', 1)
    body = entry[entry.index('{') + 1:entry.rindex('}')]
    call = re.search(r'(Pass\d+_\w+)\(', body)
    body = f'{call[1]}(at, vec3<u32>(0u));'
    return source + f'\nfn stage_{suffix}(at: vec2<u32>) {{ {body} }}\n'


def pair(a, b, first, second):
    """Keep both stages' arithmetic and their original texture quantization."""
    assert set(second['inputs']) == set(first['outputs'])
    a, b = stage(rename(a, 'a'), 'a'), stage(rename(b, 'b'), 'b')
    inputs, outputs = first['inputs'], second['outputs']
    samplers = first['samplers']
    assert samplers == second['samplers']
    a_decls, b_decls = {int(m[1]): m[2] for m in DECL.finditer(a)}, {int(m[1]): m[2] for m in DECL.finditer(b)}
    a_out = {a_decls[len(inputs) + 1 + i]: i for i in range(len(first['outputs']))}
    b_in = {b_decls[i + 1]: first['outputs'].index(name) for i, name in enumerate(second['inputs'])}
    caches, helpers = [], []
    for i, fmt in enumerate(first['formats']):
        ty = 'u32' if fmt == 'rgba8' else 'vec2<u32>'
        caches.append(f'var<workgroup> tile_{i}: array<{ty}, 324>;')
        decode = f'unpack4x8unorm(tile_{i}[index])' if fmt == 'rgba8' else f'vec4<f32>(unpack2x16float(tile_{i}[index].x), unpack2x16float(tile_{i}[index].y))'
        encode = 'pack4x8unorm(value)' if fmt == 'rgba8' else 'vec2<u32>(pack2x16float(value.xy), pack2x16float(value.zw))'
        helpers.append(f'''fn store_{i}(value: vec4<f32>) {{ tile_{i}[tile_index] = {encode}; }}
fn load_{i}(uv: vec2<f32>) -> vec4<f32> {{
    let size = Params_a0.dimensions_a0.xy;
    let at = clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
    let local = vec2<u32>(at - tile_origin);
    let index = local.y * 18u + local.x;
    return {decode};
}}''')
    a = calls(a, 'textureStore', lambda args: f'store_{a_out[args[0].strip("() ")]}({args[2]})')
    b = calls(b, 'textureSampleLevel', lambda args: f'load_{b_in[args[0].strip("() ")]}({args[2]})')

    def bind_a(m):
        binding = int(m[1])
        if m[2] in a_out:
            return ''
        if binding > len(inputs):
            binding += len(outputs) - len(first['outputs'])
        return m[0].replace(f'@binding({m[1]})', f'@binding({binding})')

    def bind_b(m):
        binding = int(m[1])
        if binding <= len(second['inputs']) or 'sampler' in m[3]:
            return ''
        binding += len(inputs) - len(second['inputs'])
        return m[0].replace(f'@binding({m[1]})', f'@binding({binding})')

    a, b = DECL.sub(bind_a, a), DECL.sub(bind_b, b)
    b = re.sub(r'struct SLANG_ParameterGroup_Params[^}]+};', '', b)
    b = b.replace('Params_b0', 'Params_a0').replace('dimensions_b0', 'dimensions_a0').replace('origin_b0', 'origin_a0')
    enable = 'enable f16;\n' if 'enable f16;' in a else ''
    a, b = a.replace('enable f16;', ''), b.replace('enable f16;', '')
    main = '''
var<private> tile_origin: vec2<i32>;
var<private> tile_index: u32;
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let origin = group.xy * 16u + Params_a0.origin_a0.xy;
    tile_origin = vec2<i32>(origin) - vec2<i32>(1);
    for (var i = lane; i < 324u; i += 64u) {
        tile_index = i;
        let at = clamp(tile_origin + vec2<i32>(vec2<u32>(i % 18u, i / 18u)), vec2<i32>(0), vec2<i32>(Params_a0.dimensions_a0.xy) - vec2<i32>(1));
        stage_a(vec2<u32>(at));
    }
    workgroupBarrier();
    for (var i = lane; i < 256u; i += 64u) {
        stage_b(origin + vec2<u32>(i % 16u, i / 16u));
    }
}
'''
    return enable + a + b + '\n'.join(caches + helpers) + main


def graph(passes):
    """Version overwritten tensors before fusion, preventing in-place races."""
    current = {'INPUT': 'INPUT'}
    result = []
    for i, p in enumerate(passes):
        value = dict(p, inputs=[current[n] for n in p['inputs']], radius=1)
        value['outputs'] = [n if n == 'OUTPUT' else f'{n}_version{i}' for n in p['outputs']]
        current.update(zip(p['outputs'], value['outputs']))
        result.append(value)
    return result


def emit(root, destination, models, allocate, matrix=False):
    """Emit fused graphs using the same allocator as the reference models."""
    import json
    name = 'matrix_model' if matrix else 'fused_model'
    rust = [f'pub(super) fn {name}(value: Upscaler) -> Model {{ match value {{',
            'Upscaler::Off => unreachable!(),']
    for variant, name in models:
        passes = json.loads((root / name / 'passes.json').read_text())
        if matrix and (passes[0]['formats'] != ['rgba16f'] * len(passes[0]['formats'])
                       or 'enable f16;' not in (root / name / '1.wgsl').read_text()):
            rust.append(f'Upscaler::{variant} => fused_model(value),')
            continue
        versions = graph(passes)
        combined, index = [], 0
        while index < len(passes):
            p = passes[index]
            value = dict(versions[index], shader=f'{name}/{index + 1}.wgsl')
            # Quantized tensors amplify accumulated rounding differences. Fuse
            # only the final intermediate pair before an 8-bit model's output.
            if (index > 0 and index + 2 < len(passes)
                    and ('rgba8' not in p['formats'] or index + 3 == len(passes))
                    and set(passes[index + 1]['inputs']) == set(p['outputs'])
                    and not any(set(versions[index]['outputs']) & set(later['inputs']) for later in versions[index + 2:])
                    and p['samplers'] == passes[index + 1]['samplers']):
                b = passes[index + 1]
                source = pair((root / name / f'{index + 1}.wgsl').read_text(),
                              (root / name / f'{index + 2}.wgsl').read_text(), p, b)
                shader = f'{name}/{index + 1}-{index + 2}.wgsl'
                (destination / shader).write_text('// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.\n' + source)
                value.update(outputs=versions[index + 1]['outputs'], formats=b['formats'],
                             block=16, radius=2, shader=shader)
                index += 1
            if matrix and index == 0:
                import matrix as cooperative
                shader = f'{name}/1-matrix.wgsl'
                source = cooperative.first((root / name / '1.wgsl').read_text())
                (destination / shader).write_text('// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.\n' + source + '\n')
                value.update(shader=shader, block=16)
            combined.append(value)
            index += 1
        names, textures = allocate(combined)
        rust.append(f'Upscaler::{variant} => Model {{ textures: &[')
        for fmt, scale in textures:
            rust.append(f"({('Rgba8Unorm' if fmt == 'rgba8' else 'Rgba16Float')}, {scale}),")
        rust.append(f"], output: {names['OUTPUT']}, passes: &[")
        for p in combined:
            source = (destination / p['shader']).read_text()
            bindings = ['Uniform']
            bindings += ['Source' if n == 'INPUT' else f'Texture({names[n]})' for n in p['inputs']]
            bindings += [f'Texture({names[n]})' for n in p['outputs']]
            bindings += ['Linear' if f == 'LINEAR' else 'Nearest' for f, _ in p['samplers']]
            used = {int(n) for n in re.findall(r'@binding\((\d+)\)', source)}
            entries = ','.join(f'({n},{value})' for n, value in enumerate(bindings) if n in used)
            rust.append(f'''Pass {{ source: include_str!("{p['shader']}"), bindings: &[{entries}], block: {p['block']}, scale: {2 if 'OUTPUT' in p['outputs'] else 1}, radius: {p['radius']} }},''')
        rust.append('] },')
    rust.append('} }')
    return '\n'.join(rust)
