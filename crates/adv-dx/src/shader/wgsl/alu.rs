use super::{LANES, *};

pub(super) const HELPERS: &str = r#"
fn dx_flush(bits: u32) -> u32 {
    return select(bits, bits & 0x80000000u, (bits & 0x7f800000u) == 0u);
}
fn dx_flush4(bits: vec4<u32>) -> vec4<u32> {
    return select(bits, bits & vec4<u32>(0x80000000u), (bits & vec4<u32>(0x7f800000u)) == vec4<u32>(0u));
}
fn dx_nan(bits: u32) -> bool {
    return (bits & 0x7fffffffu) > 0x7f800000u;
}
fn dx_sat(bits: u32) -> u32 {
    // Only positive normals and infinity, 0x00800000..=0x7f800000, survive.
    return select(0u, min(bits, 0x3f800000u), bits - 0x00800000u <= 0x7f000000u);
}
fn dx_min(a: u32, b: u32) -> u32 {
    if dx_nan(a) { return b; }
    if dx_nan(b) { return a; }
    return select(a, b, bitcast<f32>(b) < bitcast<f32>(a));
}
fn dx_max(a: u32, b: u32) -> u32 {
    if dx_nan(a) { return b; }
    if dx_nan(b) { return a; }
    return select(a, b, bitcast<f32>(b) > bitcast<f32>(a));
}
fn dx_ftoi(a: u32) -> u32 {
    let x = bitcast<f32>(a);
    if dx_nan(a) { return 0u; }
    if x >= 2147483648.0 { return 0x7fffffffu; }
    if x <= -2147483648.0 { return 0x80000000u; }
    return bitcast<u32>(i32(x));
}
fn dx_ftou(a: u32) -> u32 {
    let x = bitcast<f32>(a);
    if dx_nan(a) || x <= 0.0 { return 0u; }
    if x >= 4294967296.0 { return 0xffffffffu; }
    return u32(x);
}
fn dx_round(a: u32, mode: u32) -> u32 {
    if (a & 0x7fffffffu) >= 0x4b000000u { return a; }
    let x = bitcast<f32>(a);
    var rounded: f32;
    switch mode {
        case 0u: {
            let down = floor(x);
            let fraction = x - down;
            rounded = select(down, down + 1.0, fraction > 0.5 || (fraction == 0.5 && (i32(down) & 1) != 0));
        }
        case 1u: { rounded = floor(x); }
        case 2u: { rounded = ceil(x); }
        default: { rounded = trunc(x); }
    }
    return select(bitcast<u32>(rounded), a & 0x80000000u, rounded == 0.0);
}
fn dx_rcp(a: u32) -> u32 {
    if (a & 0x7fffffffu) == 0u { return a | 0x7f800000u; }
    return bitcast<u32>(1.0 / bitcast<f32>(a));
}
fn dx_rsq(a: u32) -> u32 {
    if (a & 0x7fffffffu) == 0u { return a | 0x7f800000u; }
    if a >= 0x80000000u { return 0x7fc00000u; }
    return bitcast<u32>(inverseSqrt(bitcast<f32>(a)));
}
fn dx_sqrt(a: u32) -> u32 {
    if (a & 0x7fffffffu) == 0u { return a; }
    if a >= 0x80000000u { return 0x7fc00000u; }
    return bitcast<u32>(sqrt(bitcast<f32>(a)));
}
fn dx_log(a: u32) -> u32 {
    if (a & 0x7fffffffu) == 0u { return 0xff800000u; }
    if a >= 0x80000000u { return 0x7fc00000u; }
    return bitcast<u32>(log2(bitcast<f32>(a)));
}
fn dx_sincos(a: u32, cosine: bool) -> u32 {
    if (a & 0x7fffffffu) >= 0x7f800000u { return 0x7fc00000u; }
    let x = bitcast<f32>(a);
    if cosine { return bitcast<u32>(cos(x)); }
    return bitcast<u32>(sin(x));
}
fn dx_f32tof16(a: u32) -> u32 {
    let sign = (a >> 16u) & 0x8000u;
    let mantissa = a & 0x007fffffu;
    let exponent = (a >> 23u) & 255u;
    if exponent == 255u {
        return sign | select(0x7c00u, 0x7e00u | (mantissa >> 13u), mantissa != 0u);
    }
    if exponent >= 143u { return sign | 0x7bffu; }
    if exponent >= 113u { return sign | ((exponent - 112u) << 10u) | (mantissa >> 13u); }
    if exponent >= 103u { return sign | ((mantissa | 0x00800000u) >> (126u - exponent)); }
    return sign;
}
fn dx_f16tof32(a: u32) -> u32 {
    let sign = (a & 0x8000u) << 16u;
    let mantissa = a & 1023u;
    let exponent = (a >> 10u) & 31u;
    if exponent == 0u {
        if mantissa == 0u { return sign; }
        let shift = countLeadingZeros(mantissa) - 21u;
        return sign | ((113u - shift) << 23u) | (((mantissa << shift) & 1023u) << 13u);
    }
    if exponent == 31u { return sign | 0x7f800000u | (mantissa << 13u); }
    return sign | ((exponent + 112u) << 23u) | (mantissa << 13u);
}
fn dx_udiv(a: u32, b: u32, remainder: bool) -> u32 {
    if b == 0u { return 0xffffffffu; }
    return select(a / b, a % b, remainder);
}
fn dx_umul_hi(a: u32, b: u32) -> u32 {
    let low = (a & 0xffffu) * (b & 0xffffu);
    let cross = (a >> 16u) * (b & 0xffffu) + (low >> 16u);
    let carry = (a & 0xffffu) * (b >> 16u) + (cross & 0xffffu);
    return (a >> 16u) * (b >> 16u) + (cross >> 16u) + (carry >> 16u);
}
fn dx_imul_hi(a: u32, b: u32) -> u32 {
    return dx_umul_hi(a, b) - select(0u, b, (a >> 31u) != 0u) - select(0u, a, (b >> 31u) != 0u);
}
fn dx_extract(a: u32, b: u32, c: u32, signed: bool) -> u32 {
    let start = b & 31u;
    let count = min(a & 31u, 32u - start);
    if signed { return bitcast<u32>(extractBits(bitcast<i32>(c), start, count)); }
    return extractBits(c, start, count);
}
fn dx_insert(a: u32, b: u32, c: u32, d: u32) -> u32 {
    let start = b & 31u;
    return insertBits(d, c, start, min(a & 31u, 32u - start));
}
fn dx_msad(a: u32, b: u32, acc: u32) -> u32 {
    var sum = 0u;
    for (var shift = 0u; shift < 32u; shift += 8u) {
        let reference = (a >> shift) & 255u;
        let value = (b >> shift) & 255u;
        sum += select(0u, max(value, reference) - min(value, reference), reference != 0u);
    }
    return acc + min(sum, 0xffffffffu - acc);
}
"#;

pub(super) fn expression(
    op: Opcode,
    args: &[String],
    dst: usize,
    lane: usize,
) -> Result<String, Error> {
    let lane = LANES.as_bytes()[lane] as char;
    let arg = |i| {
        args.get(i)
            .map_or_else(String::new, |value: &String| format!("{value}.{lane}"))
    };
    let (a, b, c, d) = (arg(0), arg(1), arg(2), arg(3));
    let (x, y, z) = (
        format!("bitcast<f32>({a})"),
        format!("bitcast<f32>({b})"),
        format!("bitcast<f32>({c})"),
    );
    let (i, j) = (format!("bitcast<i32>({a})"), format!("bitcast<i32>({b})"));
    let float = |expr: String| format!("bitcast<u32>({expr})");
    let truth = |cond: String| format!("select(0u, 0xffffffffu, {cond})");
    use Opcode::*;
    Ok(match op {
        Mov => a,
        Movc => format!("select({c}, {b}, {a} != 0u)"),
        Swapc if dst == 0 => format!("select({b}, {c}, {a} != 0u)"),
        Swapc => format!("select({c}, {b}, {a} != 0u)"),
        Add => float(format!("{x} + {y}")),
        Div => float(format!("{x} / {y}")),
        Mul => float(format!("{x} * {y}")),
        Mad => float(format!("{x} * {y} + {z}")),
        Dp2 | Dp3 | Dp4 => {
            let (len, swizzle) = if op == Dp2 {
                (2, "xy")
            } else if op == Dp3 {
                (3, "xyz")
            } else {
                (4, "xyzw")
            };
            float(format!(
                "dot(bitcast<vec{len}<f32>>({}.{swizzle}), bitcast<vec{len}<f32>>({}.{swizzle}))",
                args[0], args[1]
            ))
        }
        Eq => truth(format!("{x} == {y} && !dx_nan({a}) && !dx_nan({b})")),
        Ne => truth(format!("{x} != {y} || dx_nan({a}) || dx_nan({b})")),
        Lt => truth(format!("{x} < {y} && !dx_nan({a}) && !dx_nan({b})")),
        Ge => truth(format!("{x} >= {y} && !dx_nan({a}) && !dx_nan({b})")),
        Exp => float(format!("exp2({x})")),
        Frc => float(format!("{x} - floor({x})")),
        Ftoi => format!("dx_ftoi({a})"),
        Ftou => format!("dx_ftou({a})"),
        Log => format!("dx_log({a})"),
        Min => format!("dx_min({a}, {b})"),
        Max => format!("dx_max({a}, {b})"),
        RoundNe => format!("dx_round({a}, 0u)"),
        RoundNi => format!("dx_round({a}, 1u)"),
        RoundPi => format!("dx_round({a}, 2u)"),
        RoundZ => format!("dx_round({a}, 3u)"),
        Rcp => format!("dx_rcp({a})"),
        Rsq => format!("dx_rsq({a})"),
        Sqrt => format!("dx_sqrt({a})"),
        Sincos => format!("dx_sincos({a}, {})", dst == 1),
        F32ToF16Rtz => format!("dx_f32tof16({a})"),
        F16ToF32 => format!("dx_f16tof32({a})"),
        Iadd => format!("({a} + {b})"),
        Ieq => truth(format!("{a} == {b}")),
        Ine => truth(format!("{a} != {b}")),
        Ige => truth(format!("{i} >= {j}")),
        Ilt => truth(format!("{i} < {j}")),
        Imad | Umad => format!("({a} * {b} + {c})"),
        Imax => format!("bitcast<u32>(max({i}, {j}))"),
        Imin => format!("bitcast<u32>(min({i}, {j}))"),
        Imul if dst == 0 => format!("dx_imul_hi({a}, {b})"),
        Umul if dst == 0 => format!("dx_umul_hi({a}, {b})"),
        Imul | Umul => format!("({a} * {b})"),
        Ineg => format!("(0u - {a})"),
        Ishl => format!("({a} << ({b} & 31u))"),
        Ishr => format!("bitcast<u32>({i} >> ({b} & 31u))"),
        Ushr => format!("({a} >> ({b} & 31u))"),
        Itof => float(format!("f32({i})")),
        Utof => float(format!("f32({a})")),
        And => format!("({a} & {b})"),
        Not => format!("(~{a})"),
        Or => format!("({a} | {b})"),
        Xor => format!("({a} ^ {b})"),
        Udiv => format!("dx_udiv({a}, {b}, {})", dst == 1),
        Ult => truth(format!("{a} < {b}")),
        Uge => truth(format!("{a} >= {b}")),
        Umax => format!("max({a}, {b})"),
        Umin => format!("min({a}, {b})"),
        Uaddc if dst == 1 => format!("select(0u, 1u, ({a} + {b}) < {a})"),
        Uaddc => format!("({a} + {b})"),
        Usubb if dst == 1 => format!("select(0u, 1u, {a} < {b})"),
        Usubb => format!("({a} - {b})"),
        Countbits => format!("countOneBits({a})"),
        FirstbitHi => format!("select(countLeadingZeros({a}), 0xffffffffu, {a} == 0u)"),
        FirstbitLo => format!("select(countTrailingZeros({a}), 0xffffffffu, {a} == 0u)"),
        FirstbitShi => format!(
            "select(countLeadingZeros({a} ^ select(0u, 0xffffffffu, {i} < 0)), 0xffffffffu, {a} == 0u || {a} == 0xffffffffu)"
        ),
        Bfrev => format!("reverseBits({a})"),
        Ubfe | Ibfe => format!("dx_extract({a}, {b}, {c}, {})", op == Ibfe),
        Bfi => format!("dx_insert({a}, {b}, {c}, {d})"),
        Msad => format!("dx_msad({a}, {b}, {c})"),
        DerivRtx => float(format!("dpdx({x})")),
        DerivRty => float(format!("dpdy({x})")),
        DerivRtxCoarse => float(format!("dpdxCoarse({x})")),
        DerivRtyCoarse => float(format!("dpdyCoarse({x})")),
        DerivRtxFine => float(format!("dpdxFine({x})")),
        DerivRtyFine => float(format!("dpdyFine({x})")),
        other => return Err(Error::UnsupportedOpcode(other)),
    })
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use adv_device::gpu::{
        BufferRange, DispatchArguments, Gpu, ModernShader, ModernTransfer, SlotBindings,
        StorageBinding,
    };
    use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
    use std::sync::Arc;

    /// Every sign, exponent and leading mantissa bits, with low bits that
    /// reach each end of the mantissa.
    fn inputs() -> Vec<u32> {
        (0..=0xffffu32)
            .flat_map(|high| {
                [0, 1, 0x4000, 0x7fff, 0x8000, 0xffff].map(move |low| high << 16 | low)
            })
            .collect()
    }

    /// Denormals flush to zero, NaN and nonpositive values saturate to zero.
    fn saturated(bits: u32) -> u32 {
        let value = f32::from_bits(if bits & 0x7f80_0000 == 0 { 0 } else { bits });
        if value > 0.0 {
            value.min(1.0).to_bits()
        } else {
            0
        }
    }

    // spec: GFX-NUM-1
    #[test]
    fn saturation_helper_matches_the_reference_on_the_gpu() {
        let mut gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
        let words = inputs();
        let size = words.len() as u64 * 4;
        let buffer = |usage, memory| {
            let buffer = gpu
                .device()
                .create_buffer(&BufferDesc {
                    size,
                    usage,
                    memory,
                    label: "saturation oracle",
                })
                .unwrap();
            BufferRange {
                buffer,
                range: 0..size,
            }
        };
        let data = buffer(BufferUsage::STORAGE | BufferUsage::COPY_SRC, Memory::Upload);
        // The buffer has not been submitted.
        unsafe { &mut *data.buffer.mapped().unwrap().as_ptr() }
            .copy_from_slice(bytemuck::cast_slice(&words));
        let readback = buffer(BufferUsage::COPY_DST, Memory::Readback);
        let source = format!(
            "{}
@group(3) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if id.x < arrayLength(&data) {{ data[id.x] = dx_sat(data[id.x]); }}
}}",
            super::HELPERS
        );
        let shader =
            ModernShader::wgsl(1, &source, "main", adv_gpu::shader::Stage::Compute).unwrap();
        let pipeline = gpu.prepare_dispatch(Arc::new(shader)).unwrap();
        let slots = SlotBindings {
            storage: [StorageBinding::Buffer(data.clone())].into(),
            ..Default::default()
        };
        let groups = (words.len() as u32).div_ceil(64);
        gpu.dispatch(&pipeline, &slots, DispatchArguments::Direct([groups, 1, 1]))
            .unwrap();
        gpu.transfer(ModernTransfer::CopyBuffer {
            source: data,
            destination: readback.buffer.clone(),
            offset: 0,
        })
        .unwrap();
        let fence = gpu.target(1, 1, false).unwrap();
        gpu.clear(&fence, Some([0.; 4]), None).unwrap();
        let mut pending = gpu.readback(&fence.image, None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(result) = pending.poll() {
                result.unwrap();
                break;
            }
            gpu.check().unwrap();
            assert!(std::time::Instant::now() < deadline, "readback timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // The readback above waited for the copy.
        let results: &[u32] =
            bytemuck::cast_slice(unsafe { readback.buffer.mapped().unwrap().as_ref() });
        for (&input, &output) in words.iter().zip(results) {
            assert_eq!(output, saturated(input), "dx_sat({input:08x})");
        }
    }
}
