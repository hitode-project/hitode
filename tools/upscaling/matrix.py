"""Lower pinned RGB convolutions to half-precision 8x8 cooperative matrices.

Imported by generate.py. Input is its first-pass WGSL; output keeps the original
weights, accumulation order, activation and storage format in a tiled shader.
"""
import re
from fuse import arguments, calls, DECL


def convolution(args):
    """Flatten the nested products from the bias outwards, without reassociation."""
    terms = []
    while True:
        value, weights, accumulator = args
        assert weights.startswith('mat3x4<f16>(')
        weights, _ = arguments(weights, len('mat3x4<f16>('))
        assert len(weights) == 12
        terms.append((value, weights))
        if not accumulator.startswith('MulAdd_0('):
            bias, _ = arguments(accumulator, len('vec4<f16>('))
            assert len(bias) == 4
            return list(reversed(terms)), bias
        args, _ = arguments(accumulator, len('MulAdd_0('))


def first(source):
    """Cache the 18x18 RGB tile once; each subgroup computes eight RGB pixels."""
    body = source.split('fn Pass1_0', 1)[1].split('@compute', 1)[0]
    convolutions = []

    def product(args):
        convolutions.append(convolution(args))
        return f'result_{len(convolutions) - 1}'

    body = calls(body, 'MulAdd_0', product)
    aliases = dict(re.findall(r'var (\w+)\s*:\s*[^;=]+ = ([^;]+);', body))

    def expand(value):
        return re.sub(r'\b\w+\b', lambda m: expand(aliases[m[0]]) if m[0] in aliases else m[0], value)

    stores = []
    calls(body, 'textureStore', lambda args: stores.append((args[0], expand(args[2]))) or '')
    assert len(stores) == len(convolutions)
    order = [v for v, _ in convolutions[0][0]]
    assert len(order) == 9
    assert all([v for v, _ in terms] == order for terms, _ in convolutions)
    offset = {'_S11': -1, 'i_0': 0, 'i_0 + u32(1)': 1,
              '_S12': -1, 'j_0': 0, '_S14': 1}
    offsets = [tuple(offset[n] for n in re.findall(r'\[([^]]+)\]', v)) for v in order]
    assert set(offsets) == {(x, y) for x in (-1, 0, 1) for y in (-1, 0, 1)}
    chunks = (len(convolutions) + 1) // 2
    weights, biases = [], []
    for chunk in range(chunks):
        for term in range(9):
            for k in range(8):
                for n in range(8):
                    output = chunk * 2 + n // 4
                    weights.append(convolutions[output][0][term][1][k * 4 + n % 4]
                                   if output < len(convolutions) and k < 3 else '0.0h')
        for _ in range(8):
            for n in range(8):
                output = chunk * 2 + n // 4
                biases.append(convolutions[output][1][n % 4] if output < len(convolutions) else '0.0h')
    declarations = '\n'.join(m[0] for m in DECL.finditer(source))
    uniform = re.search(r'struct SLANG_ParameterGroup_Params[^}]+};', source)[0]
    input_name = next(m[2] for m in DECL.finditer(source) if int(m[1]) == 1)
    # Keep the original sampler live in the inferred layout.
    sampler = next(m[2] for m in DECL.finditer(source) if m[3].strip() == 'sampler')
    constants = weights + biases
    pieces = [f'''enable f16;
enable wgpu_cooperative_matrix;
{uniform}
{declarations}
const coefficients = array<f16, {len(constants)}>({','.join(constants)});
var<workgroup> weights: array<f16, {len(constants)}>;
var<workgroup> pixels: array<f16, 2592>;
var<workgroup> results: array<f16, 1024>;
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) group: vec3<u32>,
        @builtin(local_invocation_index) lane: u32,
        @builtin(subgroup_id) subgroup: u32,
        @builtin(subgroup_invocation_id) sublane: u32,
        @builtin(num_subgroups) groups: u32) {{
    let origin = group.xy * 16u + Params_0.origin_0.xy;
    let size = Params_0.dimensions_0.xy;
    for (var i = lane; i < {len(constants)}u; i += 64u) {{ weights[i] = coefficients[i]; }}
    for (var i = lane; i < 324u; i += 64u) {{
        let at = clamp(vec2<i32>(origin) + vec2<i32>(i32(i % 18u), i32(i / 18u)) - vec2<i32>(1), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
        let rgb = vec3<f16>(textureSampleLevel({input_name}, {sampler}, (vec2<f32>(at) + vec2<f32>(0.5)) / vec2<f32>(size), 0.0).rgb);
        pixels[i * 8u] = rgb.r; pixels[i * 8u + 1u] = rgb.g; pixels[i * 8u + 2u] = rgb.b;
    }}
    workgroupBarrier();
    for (var segment = subgroup; segment < 32u; segment += groups) {{
        let x = (segment % 2u) * 8u;
        let y = segment / 2u;
''']
    for chunk in range(chunks):
        pieces.append(f'var sum_{chunk} = coopLoadT<coop_mat8x8<f16,C>>(&weights[{len(weights) + chunk * 64}], 8u);')
        for term, (dx, dy) in enumerate(offsets):
            pieces.append(f'''{{
    let a = coopLoadT<coop_mat8x8<f16,A>>(&pixels[((y + {dy + 1}u) * 18u + x + {dx + 1}u) * 8u], 8u);
    let b = coopLoadT<coop_mat8x8<f16,B>>(&weights[{(chunk * 9 + term) * 64}], 8u);
    sum_{chunk} = coopMultiplyAdd(a, b, sum_{chunk});
}}''')
        pieces.append(f'''coopStoreT(sum_{chunk}, &results[subgroup * 64u], 8u);
subgroupBarrier();
if (sublane < 8u) {{
    let at = origin + vec2<u32>(x + sublane, y);
    let base = subgroup * 64u + sublane * 8u;
    if (all(at < size)) {{''')
        for index in range(chunk * 2, min(chunk * 2 + 2, len(convolutions))):
            lanes = ','.join(f'results[base + {index % 2 * 4 + c}u]' for c in range(4))
            pieces.append(f'let result_{index} = vec4<f16>({lanes});')
        for texture, value in stores:
            index = int(re.search(r'result_(\d+)', value)[1])
            if index // 2 == chunk:
                pieces.append(f'textureStore({texture}, at, {value});')
        pieces.append('} }\nsubgroupBarrier();')
    pieces.append('} }')
    return '\n'.join(pieces)
