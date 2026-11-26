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
            channel(sampler.color[lane], format.channels[lane])
        });
        let value = channels.map(|channel| format!("bitcast<f32>({}u)", channel.to_bits()));
        if matches!(opcode, SampleC | SampleCLz) {
            let reference = extra(1);
            return format!(
                "({native} + ({weight}) * ({} - {}))",
                compare(sampler.comparison, &reference, &value[0]),
                compare(sampler.comparison, &reference, "0.0")
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

    pub(super) fn border_gather(
        &mut self,
        coord: &str,
        texture: &str,
        [x, y]: [i32; 2],
        axes: [bool; 3],
        (border, scalar): (&str, &str),
        sample: &str,
    ) -> String {
        let name = format!("gather_border_{}", self.serial);
        let size = format!("{name}_size");
        let origin = format!("{name}_origin");
        self.line(&format!(
            "let {size} = vec2<f32>(textureDimensions({texture}, 0u));"
        ));
        self.line(&format!(
            "let {origin} = floor({coord} * {size} - vec2<f32>(0.5)) + vec2<f32>({x}.0, {y}.0);"
        ));
        let axes = [axes[0], axes[1]];
        let masks = [(0, 1), (1, 1), (1, 0), (0, 0)].map(|(u, v)| {
            [("x", u), ("y", v)]
                .into_iter()
                .zip(axes)
                .filter(|(_, border)| *border)
                .map(|((axis, offset), _)| {
                    let tap = format!("({origin}.{axis} + {offset}.0)");
                    format!("({tap} < 0.0 || {tap} >= {size}.{axis})")
                })
                .collect::<Vec<_>>()
                .join(" || ")
        });
        if axes == [false, false] {
            sample.into()
        } else {
            format!(
                "select({sample}, vec4<{scalar}>({scalar}({border})), vec4<bool>({}))",
                masks.join(", ")
            )
        }
    }
}

pub(super) fn channel(value: f32, channel: BorderChannel) -> f32 {
    let quantize =
        |max: u32, lower: f32| (value.clamp(lower, 1.) * max as f32).round_ties_even() / max as f32;
    match channel {
        BorderChannel::Missing => 0.0,
        BorderChannel::Float => value,
        BorderChannel::Unorm(bits) => quantize(u32::MAX >> (32 - bits), 0.),
        BorderChannel::Snorm(bits) => quantize(u32::MAX >> (33 - bits), -1.),
    }
}

pub(super) fn compare(comparison: i32, reference: &str, texel: &str) -> String {
    let op = match comparison {
        1 => return "0.0".into(),
        2 => "<",
        3 => "==",
        4 => "<=",
        5 => ">",
        6 => "!=",
        7 => ">=",
        8 => return "1.0".into(),
        _ => unreachable!(),
    };
    format!("select(0.0, 1.0, {reference} {op} {texel})")
}
