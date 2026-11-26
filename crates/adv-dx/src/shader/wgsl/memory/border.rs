//! Add a D3D border value to the native zero-border sample.

use super::*;

pub(super) struct BorderSample<'a> {
    pub sampler: BorderSampler,
    pub format: BorderTexture,
    pub depth: bool,
    pub width: usize,
    pub texture: &'a str,
    pub coordinate: &'a str,
    pub arguments: &'a [String],
    pub offset: Option<[i32; 3]>,
    pub opcode: Opcode,
    pub lod: Option<TextureLod>,
    pub native: &'a str,
}

impl Writer<'_> {
    pub(super) fn border_sample(&mut self, sample: BorderSample<'_>) -> String {
        use Opcode::*;
        let BorderSample {
            sampler,
            format,
            depth,
            width,
            texture,
            coordinate,
            arguments,
            offset,
            opcode,
            lod,
            native,
        } = sample;
        let name = format!("border_{}", self.serial);
        let coord = format!("{name}_coord");
        self.line(&format!("let {coord} = {coordinate};"));
        let (float_ty, lanes) = match width {
            1 => ("f32", "x"),
            2 => ("vec2<f32>", "xy"),
            _ => ("vec3<f32>", "xyz"),
        };
        let lod_from_grad = |x: String, y: String| {
            let size = format!("{float_ty}(textureDimensions({texture}, 0u))");
            let magnitudes = if width == 1 {
                format!("max(abs({x} * {size}), abs({y} * {size}))")
            } else {
                format!("max(length({x} * {size}), length({y} * {size}))")
            };
            format!(
                "log2({magnitudes}) + bitcast<f32>({}u)",
                sampler.lod.bias.to_bits()
            )
        };
        let extra = |i: usize| format!("bitcast<f32>({}.x)", arguments[i]);
        let raw = match opcode {
            SampleL => extra(1),
            SampleCLz => "0.0".into(),
            SampleD => lod_from_grad(
                format!("bitcast<{float_ty}>({}.{})", arguments[1], lanes),
                format!("bitcast<{float_ty}>({}.{})", arguments[2], lanes),
            ),
            _ => {
                let base = lod_from_grad(
                    format!("dpdxCoarse({coord})"),
                    format!("dpdyCoarse({coord})"),
                );
                if opcode == SampleB {
                    format!("({base}) + {}", extra(1))
                } else {
                    base
                }
            }
        };
        let minimum = sampler.lod.min.max(lod.map_or(0., |lod| lod.minimum));
        let maximum = sampler.lod.max.max(minimum);
        let level = format!("{name}_lod");
        self.line(&format!(
            "let {level} = min(clamp({raw}, bitcast<f32>({}u), bitcast<f32>({}u)), f32(textureNumLevels({texture}) - 1u));",
            minimum.to_bits(), maximum.to_bits()
        ));
        let offset = offset.unwrap_or_default();
        let inside = |mip: &str| {
            let size = format!("{float_ty}(textureDimensions({texture}, {mip}))");
            let axes = (0..width)
                .filter(|&axis| sampler.axes[axis])
                .map(|axis| {
                    let axis_selector = if width == 1 {
                        String::new()
                    } else {
                        format!(".{}", LANES.as_bytes()[axis] as char)
                    };
                    let dimension = format!("{size}{axis_selector}");
                    let scaled = format!(
                        "({coord}{axis_selector} * {dimension} + {}.0)",
                        offset[axis]
                    );
                    let point =
                        format!("select(0.0, 1.0, {scaled} >= 0.0 && {scaled} < {dimension})");
                    let linear = format!(
                        "clamp(min({scaled} + 0.5, {dimension} + 0.5 - {scaled}), 0.0, 1.0)"
                    );
                    match (sampler.linear[0], sampler.linear[1]) {
                        (true, true) => linear,
                        (false, false) => point,
                        (min, _) => {
                            let (mag, min) = if min {
                                (point, linear)
                            } else {
                                (linear, point)
                            };
                            format!("select({mag}, {min}, {level} > 0.0)")
                        }
                    }
                })
                .collect::<Vec<_>>();
            if axes.is_empty() {
                "1.0".into()
            } else {
                axes.join(" * ")
            }
        };
        let weight = if sampler.linear[2] {
            let lower = format!("u32({level})");
            let upper = format!("min({lower} + 1u, textureNumLevels({texture}) - 1u)");
            format!(
                "1.0 - mix({}, {}, fract({level}))",
                inside(&lower),
                inside(&upper)
            )
        } else {
            format!("1.0 - ({})", inside(&format!("u32({level} + 0.5)")))
        };
        let channels = std::array::from_fn::<_, 4, _>(|lane| {
            let value = sampler.color[lane];
            let quantize = |max: u32, lower: f32| {
                (value.clamp(lower, 1.) * max as f32).round_ties_even() / max as f32
            };
            match format.channels[lane] {
                BorderChannel::Missing => 0f32,
                BorderChannel::Float => value,
                BorderChannel::Unorm(bits) => quantize(u32::MAX >> (32 - bits), 0.),
                BorderChannel::Snorm(bits) => quantize(u32::MAX >> (33 - bits), -1.),
            }
        });
        let value = channels.map(|channel| format!("bitcast<f32>({}u)", channel.to_bits()));
        if matches!(opcode, SampleC | SampleCLz) {
            let reference = extra(1);
            let compare = |site: &str| {
                let op = match sampler.comparison {
                    1 => return "0.0".to_owned(),
                    2 => "<",
                    3 => "==",
                    4 => "<=",
                    5 => ">",
                    6 => "!=",
                    7 => ">=",
                    8 => return "1.0".to_owned(),
                    _ => unreachable!(),
                };
                format!("select(0.0, 1.0, {reference} {op} {site})")
            };
            return format!(
                "({native} + ({weight}) * ({} - {}))",
                compare(&value[0]),
                compare("0.0")
            );
        }
        if depth {
            return format!("({native} + ({weight}) * {})", value[0]);
        }
        let color = value.join(", ");
        let result = format!("({native} + ({weight}) * vec4<f32>({color}))");
        if matches!(format.channels[3], BorderChannel::Missing) {
            format!("vec4<f32>({result}.xyz, 1.0)")
        } else {
            result
        }
    }
}
