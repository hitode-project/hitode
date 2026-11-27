//! Point-load image filtering when the native format cannot filter or resolve.

use super::*;
use adv_gpu::Format;
use std::hash::{Hash, Hasher};

impl Gpu {
    pub fn prepare_image_filter(
        &self,
        format: Format,
        volume: bool,
        resolve: bool,
    ) -> Result<Dispatch, Error> {
        let storage = match format {
            Format::R32Float => "r32float",
            Format::Rg32Float => "rg32float",
            Format::Rgba32Float => "rgba32float",
            Format::Rgba16Unorm => "rgba16unorm",
            Format::Rgba16Snorm => "rgba16snorm",
            Format::Rg16Unorm => "rg16unorm",
            Format::Rg16Snorm => "rg16snorm",
            _ => return Err(Error::Invalid("image filter format".into())),
        };
        let (size, dimension, group) = if volume { (3, "3d", 4) } else { (2, "2d", 8) };
        let lanes = if volume { "xyz" } else { "xy" };
        let sampled = if resolve {
            "multisampled_2d"
        } else {
            dimension
        };
        let operation = if resolve {
            r#"
    let samples = textureNumSamples(source);
    for (var sample_index = 0u; sample_index < samples; sample_index += 1u) {
        value += textureLoad(source, vec2<i32>(id.xy), sample_index) / f32(samples);
    }
"#
            .into()
        } else {
            let corner = (0..size)
                .map(|axis| format!("f32((tap >> {axis}u) & 1u)"))
                .collect::<Vec<_>>()
                .join(", ");
            let weight = "xyz"[..size]
                .chars()
                .map(|lane| format!("mix(1.0 - fraction.{lane}, fraction.{lane}, corner.{lane})"))
                .collect::<Vec<_>>()
                .join(" * ");
            format!(
                "
    let source_extent = textureDimensions(source);
    let position = (vec{size}<f32>(id.{lanes}) + 0.5) * vec{size}<f32>(source_extent) / vec{size}<f32>(extent) - 0.5;
    let fraction = fract(position);
    for (var tap = 0u; tap < {}u; tap += 1u) {{
        let corner = vec{size}<f32>({corner});
        let weight = {weight};
        if weight > 0.0 {{
            let texel = min(vec{size}<u32>(floor(position) + corner), source_extent - 1u);
            value += textureLoad(source, vec{size}<i32>(texel), 0) * weight;
        }}
    }}
",
                1 << size
            )
        };
        let depth = if volume { 4 } else { 1 };
        let source = format!(
            "
@group(1) @binding(0) var source: texture_{sampled}<f32>;
@group(3) @binding(0) var destination: texture_storage_{dimension}<{storage}, write>;
@compute @workgroup_size({group}, {group}, {depth})
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let extent = textureDimensions(destination);
    if any(id.{lanes} >= extent) {{ return; }}
    var value = vec4<f32>();
    {operation}
    textureStore(destination, vec{size}<i32>(id.{lanes}), value);
}}
"
        );
        self.prepare_dispatch(image_shader(&source, Stage::Compute)?)
    }
}

impl ModernDraw {
    pub fn packed_resolve(destination: ImageView, [r, g, b, a]: [u32; 4]) -> Result<Self, Error> {
        let vertex = image_shader(
            "@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                let p = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
                return vec4<f32>(p[i], 0.0, 1.0);
            }",
            Stage::Vertex,
        )?;
        let fragment = image_shader(
            &format!(
                "@group(1) @binding(0) var source: texture_multisampled_2d<f32>;
                @fragment fn main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {{
                    let samples = textureNumSamples(source);
                    var color = vec4<f32>();
                    for (var i = 0u; i < samples; i += 1u) {{
                        color += textureLoad(source, vec2<i32>(position.xy), i);
                    }}
                    let maxima = vec4<f32>({r}.0, {g}.0, {b}.0, {a}.0);
                    return round(clamp(color / f32(samples), vec4<f32>(0.0), vec4<f32>(1.0)) * maxima) / maxima;
                }}"
            ),
            Stage::Fragment,
        )?;
        let size = destination.size();
        let area = Rect {
            x: 0,
            y: 0,
            w: size.width,
            h: size.height,
        };
        Ok(Self {
            state_ids: [0; 4],
            vertex,
            fragment: Some(fragment),
            colors: [ModernColor {
                view: destination,
                blend: None,
                write_mask: 15,
                load: Load::DontCare,
                store: Store::Store,
            }]
            .into_iter()
            .collect(),
            color_slots: None,
            depth: None,
            samples: 1,
            alpha_to_coverage: false,
            primitive: p::Primitive {
                topology: p::Topology::Triangles,
                cull: p::Cull::None,
                front_ccw: false,
                unclipped_depth: false,
                wireframe: false,
            },
            area,
            viewport: [0., 0., size.width as f32, size.height as f32, 0., 1.],
            scissor: area,
            stencil_reference: 0,
            blend_constant: [0.; 4],
            depth_bias: [0.; 3],
            arguments: DrawArguments::Direct {
                vertices: 0..3,
                instances: 0..1,
                base_vertex: 0,
            },
            index: None,
            occlusion: None,
            predicate: None,
        })
    }
}

fn image_shader(source: &str, stage: Stage) -> Result<Arc<ModernShader>, Error> {
    let mut identity = std::hash::DefaultHasher::new();
    source.hash(&mut identity);
    Ok(Arc::new(ModernShader::wgsl(
        identity.finish(),
        source,
        "main",
        stage,
    )?))
}
