//! The fixed vertex stage uses the same raw input layout and uniform blocks as
//! translated shaders. Numeric state stays in uniforms so changing a matrix,
//! material or light value does not require a new pipeline.
use super::*;
use adv_device::{constants::ConstantUsage, gpu::ConstantSnapshot};
use std::fmt::Write;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Variant {
    layout: fixed::Layout,
    lights: [u8; 8],
    lit: bool,
    sources: [u32; 4],
    normalize: bool,
    local_viewer: bool,
    specular: bool,
    fog: u32,
    range_fog: bool,
    pub scaled: bool,
}

pub(super) const HELPERS: &str = r#"
fn fixed_row(p:vec4<f32>, first:u32)->vec4<f32> {
    return ((p.x*vs_constants.f[first]+p.y*vs_constants.f[first+1u])+p.z*vs_constants.f[first+2u])+p.w*vs_constants.f[first+3u];
}
fn fixed_normalize(v:vec3<f32>)->vec3<f32> {
    let magnitude=sqrt(dot(v,v));
    if magnitude==0.0 {return v;}
    return v/magnitude;
}
"#;

pub(super) fn inputs(layout: fixed::Layout) -> Vec<Input> {
    let mut inputs = transformed::inputs(layout);
    if let Some((stream, offset)) = layout.specular {
        inputs.push(Input {
            stream: stream as usize,
            offset,
            format: AttributeFormat::Bgra8Unorm,
            register: 10,
        });
    }
    if let Some((stream, offset)) = layout.normal {
        inputs.push(Input {
            stream: stream as usize,
            offset,
            format: AttributeFormat::Float3,
            register: 11,
        });
    }
    inputs
}

impl Variant {
    pub fn new(
        layout: fixed::Layout,
        f: &fixed::Fixed,
        kind: i32,
    ) -> Result<Self, fixed::Rejected> {
        let lit = !layout.transformed && f.render[&137] != 0;
        let lights = if lit {
            fixed::lighting::active(f)?.map(|light| light.map_or(0, |v| v[0] as u8))
        } else {
            [0; 8]
        };
        let sources = [145, 146, 147, 148].map(|state| {
            if lit && f.render[&141] != 0 {
                match f.render[&state] {
                    1 if layout.color.is_some() => 1,
                    2 if layout.specular.is_some() => 2,
                    _ => 0,
                }
            } else {
                0
            }
        });
        let table = f.render[&35];
        let fog = if f.render[&28] == 0 {
            0
        } else if table != 0 {
            table
        } else if layout.transformed {
            0
        } else {
            f.render[&140]
        };
        Ok(Self {
            layout,
            lights,
            lit,
            sources,
            normalize: lit && f.render[&143] != 0,
            local_viewer: lit && f.render[&142] != 0,
            specular: lit && f.render[&29] != 0,
            fog,
            range_fog: fog != 0 && table == 0 && f.render[&48] != 0,
            scaled: kind == 1 && !layout.transformed && f.render[&157] != 0,
        })
    }

    pub fn bounds(&self, f: &fixed::Fixed, count: u32, fixed_pixel: bool) -> fixed::VertexBounds {
        fixed::VertexBounds {
            transforms: (!self.layout.transformed).then_some(f.transforms),
            point_scale: self
                .scaled
                .then(|| [158, 159, 160].map(|k| f32::from_bits(f.render[&k]))),
            count,
            fixed_pixel,
        }
    }

    pub fn constants(
        &self,
        f: &fixed::Fixed,
    ) -> Result<(ConstantSnapshot, ConstantUsage), fixed::Rejected> {
        let mut usage = ConstantUsage::default();
        if self.layout.transformed {
            return Ok((ConstantSnapshot::default(), usage));
        }
        let mut values = [[0.; 4]; 70];
        for (out, row) in values.iter_mut().zip(
            f.transforms
                .iter()
                .flat_map(|matrix| matrix.chunks_exact(4)),
        ) {
            out.copy_from_slice(row);
        }
        let mut count = 12;
        if self.scaled {
            values[12] = [158, 159, 160, 154].map(|k| f32::from_bits(f.render[&k]));
            count = 13;
        }
        if self.lit {
            let normal = fixed::lighting::normal_matrix(f);
            for (out, row) in values[13..16].iter_mut().zip(normal) {
                out[..3].copy_from_slice(&row);
            }
            for (out, row) in values[16..20].iter_mut().zip(f.material.chunks_exact(4)) {
                out.copy_from_slice(row);
            }
            values[20] = fixed::lighting::color(f.render[&139]);
            values[21][0] = f.material[16];
            count = 22;
            for light in fixed::lighting::active(f)?.into_iter().flatten() {
                let position = fixed::row([light[13], light[14], light[15], 1.], &f.transforms[1]);
                let direction = fixed::row([light[16], light[17], light[18], 0.], &f.transforms[1]);
                values[count..count + 6].copy_from_slice(&[
                    light[1..5].try_into().unwrap(),
                    light[5..9].try_into().unwrap(),
                    light[9..13].try_into().unwrap(),
                    [position[0], position[1], position[2], light[19]],
                    [direction[0], direction[1], direction[2], light[20]],
                    [light[21], light[22], light[23], (light[24] * 0.5).cos()],
                ]);
                values[count + 1][3] = (light[25] * 0.5).cos();
                count += 6;
            }
        }
        for register in 0..count {
            usage.set(register);
        }
        Ok((
            ConstantSnapshot::capture(usage, |n| values[n].map(f32::to_bits)),
            usage,
        ))
    }

    pub fn emit(&self, s: &mut String, inputs: &[Input]) {
        s.push_str("let viewport=bitcast<vec4<f32>>(draw_parameters.values[0]);\n");
        if self.layout.transformed {
            s.push_str("let w=1.0/a0.w;out.position=vec4<f32>((((a0.x+0.5-viewport.z)/viewport.x)*2.0-1.0)*w,(1.0-((a0.y+0.5-viewport.w)/viewport.y)*2.0)*w,a0.z*w,w);let clip_position=out.position;\n");
        } else {
            s.push_str("let eye=fixed_row(fixed_row(vec4<f32>(a0,1.0),0u),4u);let clip_position=fixed_row(eye,8u);out.position=clip_position;out.position.x+=out.position.w/viewport.x;out.position.y-=out.position.w/viewport.y;\n");
        }
        let attribute = |register, fallback: &str| {
            inputs.iter().find(|i| i.register == register).map_or_else(
                || fallback.into(),
                |i| i.format.unpack(&format!("a{register}")),
            )
        };
        let diffuse = attribute(1, "vec4<f32>(1.0)");
        let specular = attribute(10, "vec4<f32>(0.0)");
        writeln!(s, "out.c0={diffuse};out.c1={specular};").unwrap();
        if self.lit {
            self.emit_lighting(s);
        }
        if self.fog == 0 {
            s.push_str("out.fog=out.c1.a;\n");
        } else {
            let distance = if self.layout.transformed {
                "a0.z"
            } else if self.range_fog {
                "length(eye.xyz)"
            } else {
                "abs(eye.z)"
            };
            writeln!(
                s,
                "let distance={distance};let fog=bitcast<vec4<f32>>(draw_parameters.values[11]);"
            )
            .unwrap();
            s.push_str(match self.fog {
                1 => "out.fog=exp(-fog.z*distance);\n",
                2 => "let density=fog.z*distance;out.fog=exp(-(density*density));\n",
                _ => "out.fog=clamp((fog.y-distance)/(fog.y-fog.x),0.0,1.0);\n",
            });
        }
        for n in 0..8 {
            let value = attribute(n + 2, "vec4<f32>(0.0,0.0,0.0,1.0)");
            writeln!(s, "out.t{n}={value};").unwrap();
        }
        if self.scaled {
            s.push_str("let scale=vs_constants.f[12];let point_distance=length(eye.xyz);let fixed_point_size=viewport.y*scale.w/sqrt(scale.x+scale.y*point_distance+scale.z*point_distance*point_distance);\n");
        }
    }

    fn emit_lighting(&self, s: &mut String) {
        for (n, (name, slot)) in [
            ("diffuse", 16),
            ("specular", 18),
            ("ambient", 17),
            ("emissive", 19),
        ]
        .into_iter()
        .enumerate()
        {
            let source = match self.sources[n] {
                1 => "out.c0".into(),
                2 => "out.c1".into(),
                _ => format!("vs_constants.f[{slot}]"),
            };
            writeln!(s, "let {name}={source};").unwrap();
        }
        let normal = if self.layout.normal.is_some() {
            "a11"
        } else {
            "vec3<f32>(0.0)"
        };
        writeln!(s, "let raw_normal={normal};var normal=(raw_normal.x*vs_constants.f[13].xyz+raw_normal.y*vs_constants.f[14].xyz)+raw_normal.z*vs_constants.f[15].xyz;").unwrap();
        if self.normalize {
            s.push_str("normal=fixed_normalize(normal);\n");
        }
        s.push_str("var lit=emissive.rgb+ambient.rgb*vs_constants.f[20].rgb;var highlight=vec3<f32>(0.0);\n");
        if self.specular {
            let viewer = if self.local_viewer {
                "fixed_normalize(-eye.xyz)"
            } else {
                "vec3<f32>(0.0,0.0,-1.0)"
            };
            writeln!(s, "let viewer={viewer};").unwrap();
        }
        for (n, &kind) in self.lights.iter().enumerate().take_while(|&(_, &k)| k != 0) {
            let slot = n * 6 + 22;
            writeln!(s, "{{let light_diffuse=vs_constants.f[{slot}];let light_specular=vs_constants.f[{}];let light_ambient=vs_constants.f[{}];let light_position=vs_constants.f[{}];let light_direction=vs_constants.f[{}];let falloff=vs_constants.f[{}];var attenuation=1.0;", slot + 1, slot + 2, slot + 3, slot + 4, slot + 5).unwrap();
            if kind == 3 {
                s.push_str("let direction=fixed_normalize(-light_direction.xyz);\n");
            } else {
                s.push_str("let delta=light_position.xyz-eye.xyz;let distance=length(delta);if !(distance>light_position.w) {let direction=fixed_normalize(delta);attenuation=1.0/(falloff.x+falloff.y*distance+falloff.z*distance*distance);\n");
                if kind == 2 {
                    s.push_str("let cosine=-dot(fixed_normalize(light_direction.xyz),direction);let outer=light_specular.a;var cone=1.0;if cosine<=outer {cone=0.0;}else if cosine<=falloff.w {cone=pow((cosine-outer)/(falloff.w-outer),light_direction.w);}attenuation*=cone;\n");
                }
            }
            s.push_str("let ndotl=max(dot(normal,direction),0.0);lit+=attenuation*(ambient.rgb*light_ambient.rgb+diffuse.rgb*light_diffuse.rgb*ndotl);\n");
            if self.specular {
                s.push_str("if ndotl>0.0 {let power=pow(max(dot(normal,fixed_normalize(viewer+direction)),0.0),vs_constants.f[21].x);highlight+=attenuation*specular.rgb*light_specular.rgb*power;}\n");
            }
            if kind != 3 {
                s.push_str("}\n");
            }
            s.push_str("}\n");
        }
        s.push_str("out.c0=clamp(vec4<f32>(lit,diffuse.a),vec4<f32>(0.0),vec4<f32>(1.0));out.c1=vec4<f32>(clamp(highlight,vec3<f32>(0.0),vec3<f32>(1.0)),0.0);\n");
    }
}
