//! Filter formats that can only be sampled with point filtering.

use super::*;

pub(super) struct FilterSample<'a> {
    pub op: &'a Instruction,
    pub arguments: &'a [String],
    pub texture: &'a str,
    pub slot: u32,
    pub sampler: u32,
    pub dimension: ResourceDimension,
    pub coordinate: &'a str,
    pub layer: &'a str,
    pub lod: Option<TextureLod>,
}

impl Writer<'_> {
    pub(super) fn filter_sample(&mut self, sample: FilterSample<'_>) -> Result<String, Error> {
        let FilterSample {
            op,
            arguments,
            texture,
            slot,
            sampler,
            dimension,
            coordinate,
            layer,
            lod,
        } = sample;
        use Opcode::*;
        let cube = matches!(
            dimension,
            ResourceDimension::TextureCube | ResourceDimension::TextureCubeArray
        );
        let narrow = matches!(
            dimension,
            ResourceDimension::Texture1D | ResourceDimension::Texture1DArray
        );
        let width = if dimension == ResourceDimension::Texture3D {
            3
        } else {
            2
        };
        let ty = format!("vec{width}<f32>");
        let raw_ty = if cube { "vec3<f32>" } else { &ty };
        let comparison = matches!(op.opcode, SampleC | SampleCLz);
        let depth = comparison || self.options.depth_textures.contains(&slot);
        let state = self
            .options
            .manual_samplers
            .iter()
            .find(|mode| mode.slot == sampler)
            .copied()
            .ok_or(Error::Invalid("point sampler state"))?;
        let native_sampler = self.sampler_variant(sampler, None, true);
        let result = format!("filtered_{}", self.serial);
        self.line(&format!("var {result} = vec4<f32>();"));
        self.open("");
        self.line(&format!("let coordinate = {coordinate};"));
        let argument = |i: usize| format!("bitcast<f32>({}.x)", arguments[i]);
        let aniso = state.lod.max_anisotropy;
        let raw_lod = match op.opcode {
            SampleL => argument(1),
            SampleCLz => "0.0".into(),
            Sample | SampleB | SampleC | SampleD => {
                for (index, axis, lane) in [(1, "x", "dx"), (2, "y", "dy")] {
                    let grad = if op.opcode != SampleD {
                        format!("dpd{axis}Coarse(coordinate)")
                    } else if narrow {
                        format!("vec2<f32>(bitcast<f32>({}.x), 0.0)", arguments[index])
                    } else {
                        format!(
                            "bitcast<{raw_ty}>({}.{})",
                            arguments[index],
                            if width == 3 || cube { "xyz" } else { "xy" }
                        )
                    };
                    self.line(&format!("let {lane} = {grad};"));
                    let footprint = if cube {
                        format!("dx_cube_gradient(coordinate, {lane})")
                    } else {
                        lane.to_owned()
                    };
                    self.line(&format!(
                        "let {lane}_texels = {footprint} * {ty}(textureDimensions({texture}, 0u));"
                    ));
                }
                self.line("let major = max(length(dx_texels), length(dy_texels));");
                if aniso > 1 {
                    self.line(&format!("let extent = dx_anisotropic_extent(dot(dx_texels, dx_texels), dot(dx_texels, dy_texels), dot(dy_texels, dy_texels), {aniso}.0);"));
                    self.line(&format!(
                        "let samples = u32(clamp(ceil(major / max(extent, 1.0)), 1.0, {aniso}.0));"
                    ));
                    self.line("let direction = select(dy, dx, dot(dx_texels, dx_texels) >= dot(dy_texels, dy_texels));");
                }
                let extent = if aniso > 1 { "extent" } else { "major" };
                let bias = state.lod.bias.to_bits();
                let extra = if op.opcode == SampleB {
                    format!(" + {}", argument(1))
                } else {
                    String::new()
                };
                format!("log2({extent}) + bitcast<f32>({bias}u){extra}")
            }
            _ => return Err(Error::UnsupportedOpcode(op.opcode)),
        };
        let multiple_taps = aniso > 1 && !matches!(op.opcode, SampleL | SampleCLz);
        let minimum = state
            .lod
            .min
            .max(lod.map_or(0., |view| view.minimum))
            .max(0.);
        let maximum = state.lod.max.max(minimum);
        self.line(&format!("let raw_lod = {raw_lod};"));
        self.line(&format!("let level = min(clamp(raw_lod, bitcast<f32>({}u), bitcast<f32>({}u)), f32(textureNumLevels({texture}) - 1u));", minimum.to_bits(), maximum.to_bits()));
        let [min, mag, mip] = state.linear;
        self.line(&format!(
            "let linear = select({mag}, {min}, raw_lod > 0.0);"
        ));
        if multiple_taps {
            self.open("for (var index = 0u; index < samples; index += 1u)");
            self.line("let tap_coord = coordinate + ((f32(index) + 0.5) / f32(samples) - 0.5) * direction;");
        } else {
            self.line("let tap_coord = coordinate;");
        }
        if cube {
            self.line("let face_uv = dx_filter_cube_uv(tap_coord);");
            self.line("let uv = face_uv.xy;");
        } else {
            self.line("let uv = tap_coord;");
        }
        if mip {
            self.open("for (var mip = 0u; mip < 2u; mip += 1u)");
            self.line(&format!(
                "let mip_level = min(u32(level) + mip, textureNumLevels({texture}) - 1u);"
            ));
            self.line("let mip_weight = select(fract(level), 1.0 - fract(level), mip == 0u);");
            self.open("if mip_weight > 0.0");
        } else {
            self.line("let mip_level = u32(floor(round(level * 256.0) / 256.0 + 0.5));");
            self.line("let mip_weight = 1.0;");
        }
        let offset = op.resource.offset.unwrap_or_default();
        let offsets = offset[..width]
            .iter()
            .map(|value| format!("{value}.0"))
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!(
            "let size = {ty}(textureDimensions({texture}, mip_level));"
        ));
        self.line(&format!(
            "let position = uv * size + {ty}({offsets}) - select(0.0, 0.5, linear);"
        ));
        let dimensions = if narrow { 1 } else { width };
        self.open(&format!(
            "for (var corner = 0u; corner < select(1u, {}u, linear); corner += 1u)",
            1 << dimensions
        ));
        let bits = (0..width)
            .map(|axis| {
                if axis < dimensions {
                    format!("f32((corner >> {axis}u) & 1u)")
                } else {
                    "0.0".into()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("let step = {ty}({bits});"));
        self.line("let center = floor(position) + step + 0.5;");
        let weights = (0..dimensions)
            .map(|axis| {
                let lane = LANES.as_bytes()[axis] as char;
                format!("mix(1.0 - fract(position.{lane}), fract(position.{lane}), step.{lane})")
            })
            .collect::<Vec<_>>()
            .join(" * ");
        let samples = if multiple_taps { "f32(samples)" } else { "1.0" };
        self.line(&format!(
            "let weight = select(1.0, {weights}, linear) * mip_weight / {samples};"
        ));
        self.open("if weight > 0.0");
        let level = if depth { "mip_level" } else { "f32(mip_level)" };
        let sample = |center: &str| {
            let location = if cube {
                format!("dx_filter_cube_coord(face_uv.z, {center} / size)")
            } else {
                format!("{center} / size")
            };
            format!("textureSampleLevel({texture}, {native_sampler}, {location}{layer}, {level})")
        };
        let mut texel = sample("center");
        if !cube
            && let Some(view) = self
                .options
                .border_textures
                .iter()
                .find(|view| view.slot == slot)
        {
            let axes = (0..dimensions)
                .filter(|&axis| state.axes[axis])
                .map(|axis| {
                    let lane = LANES.as_bytes()[axis] as char;
                    format!("center.{lane} < 0.0 || center.{lane} >= size.{lane}")
                })
                .collect::<Vec<_>>();
            if !axes.is_empty() {
                let color = std::array::from_fn::<_, 4, _>(|lane| {
                    format!(
                        "bitcast<f32>({}u)",
                        border::channel(state.color[lane], view.channels[lane]).to_bits()
                    )
                });
                let boundary = if depth {
                    color[0].clone()
                } else {
                    format!("vec4<f32>({})", color.join(", "))
                };
                texel = format!("select({texel}, {boundary}, {})", axes.join(" || "));
            }
        }
        let value = |texel: &str| {
            if comparison {
                format!(
                    "vec4<f32>({})",
                    border::compare(state.comparison, &argument(1), texel)
                )
            } else if depth {
                format!("vec4<f32>({texel}, 0.0, 0.0, 1.0)")
            } else {
                texel.to_owned()
            }
        };
        if cube {
            self.line("let edge = clamp(center, vec2<f32>(0.5), size - 0.5);");
            self.open("if all(center != edge)");
            let corners = [
                "edge",
                "vec2<f32>(center.x, edge.y)",
                "vec2<f32>(edge.x, center.y)",
            ]
            .map(|position| value(&sample(position)));
            self.line(&format!(
                "{result} += ({}) * (weight / 3.0);",
                corners.join(" + ")
            ));
            self.close();
            self.open("else");
        }
        self.line(&format!("{result} += {} * weight;", value(&texel)));
        if cube {
            self.close();
        }
        self.close();
        self.close();
        if mip {
            self.close();
            self.close();
        }
        if multiple_taps {
            self.close();
        }
        self.close();
        Ok(format!("bitcast<vec4<u32>>({result})"))
    }
}

pub(super) const CUBE: &str = "
fn dx_filter_cube_uv(direction: vec3<f32>) -> vec3<f32> {
    let a = abs(direction);
    var face = 0.0;
    var uv = vec2<f32>();
    if a.x >= a.y && a.x >= a.z {
        face = select(1.0, 0.0, direction.x >= 0.0);
        uv = vec2<f32>(select(direction.z, -direction.z, direction.x >= 0.0), -direction.y) / a.x;
    } else if a.y >= a.z {
        face = select(3.0, 2.0, direction.y >= 0.0);
        uv = vec2<f32>(direction.x, select(-direction.z, direction.z, direction.y >= 0.0)) / a.y;
    } else {
        face = select(5.0, 4.0, direction.z >= 0.0);
        uv = vec2<f32>(select(-direction.x, direction.x, direction.z >= 0.0), -direction.y) / a.z;
    }
    return vec3<f32>(uv * 0.5 + 0.5, face);
}

fn dx_filter_cube_coord(face: f32, uv: vec2<f32>) -> vec3<f32> {
    let p = uv * 2.0 - 1.0;
    switch u32(face) {
        case 0u: { return vec3<f32>(1.0, -p.y, -p.x); }
        case 1u: { return vec3<f32>(-1.0, -p.y, p.x); }
        case 2u: { return vec3<f32>(p.x, 1.0, p.y); }
        case 3u: { return vec3<f32>(p.x, -1.0, -p.y); }
        case 4u: { return vec3<f32>(p.x, -p.y, 1.0); }
        default: { return vec3<f32>(-p.x, -p.y, -1.0); }
    }
}
";
