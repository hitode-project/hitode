//! Scalar fixed-function vertex processing for bounds and shader comparisons.
use super::{LowerError, Render, State};
use adv_device::gpu::Viewport;

pub(crate) fn row(vector: [f32; 4], matrix: &[f32; 16]) -> [f32; 4] {
    std::array::from_fn(|column| (0..4).map(|i| vector[i] * matrix[i * 4 + column]).sum())
}

pub(crate) fn pretransformed(raw: [f32; 4], viewport: Viewport) -> Option<[f32; 4]> {
    if raw[3] == 0. || !raw[3].is_finite() {
        return None;
    }
    let w = 1. / raw[3];
    let position = [
        (((raw[0] + 0.5 - viewport.x) / viewport.width) * 2. - 1.) * w,
        (1. - ((raw[1] + 0.5 - viewport.y) / viewport.height) * 2.) * w,
        raw[2] * w,
        w,
    ];
    position.iter().all(|v| v.is_finite()).then_some(position)
}

pub(crate) fn clip(
    mut raw: [f32; 4],
    transforms: Option<&[[f32; 16]; 3]>,
    viewport: adv_device::gpu::Viewport,
) -> Option<([f32; 4], [f32; 3])> {
    if let Some([world, view, projection]) = transforms {
        raw[3] = 1.;
        let eye = row(row(raw, world), view);
        let mut clip = row(eye, projection);
        clip[0] += clip[3] / viewport.width;
        clip[1] -= clip[3] / viewport.height;
        Some((clip, [eye[0], eye[1], eye[2]]))
    } else {
        pretransformed(raw, viewport).map(|clip| (clip, [0.; 3]))
    }
}

pub(crate) fn active(f: &State) -> Result<[Option<[f32; 26]>; 8], LowerError> {
    let mut lights = [None; 8];
    if f.render[&Render::Lighting] != 0 {
        for (n, light) in f.lights.values().filter(|l| l.enabled).enumerate() {
            let Some(slot) = lights.get_mut(n) else {
                return Err(LowerError::TooManyLights);
            };
            let mut value = light.value.words().map(f32::from_bits);
            value[0] = light.value.kind as u32 as f32;
            *slot = Some(value);
        }
    }
    Ok(lights)
}
pub(crate) fn color(argb: u32) -> [f32; 4] {
    [
        ((argb >> 16) & 255) as f32 / 255.,
        ((argb >> 8) & 255) as f32 / 255.,
        (argb & 255) as f32 / 255.,
        (argb >> 24) as f32 / 255.,
    ]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|n| a[n] * b[n]).sum()
}
pub(crate) fn normal_matrix(f: &State) -> [[f32; 3]; 3] {
    let m: [[f32; 3]; 3] = std::array::from_fn(|r| {
        let row = row(
            f.transforms[0][r * 4..r * 4 + 4].try_into().unwrap(),
            &f.transforms[1],
        );
        [row[0], row[1], row[2]]
    });
    let cofactor = std::array::from_fn::<_, 3, _>(|r| {
        std::array::from_fn::<_, 3, _>(|c| {
            let (a, b) = ((r + 1) % 3, (r + 2) % 3);
            let (x, y) = ((c + 1) % 3, (c + 2) % 3);
            m[a][x] * m[b][y] - m[a][y] * m[b][x]
        })
    });
    let det = dot(m[0], cofactor[0]);
    cofactor.map(|r| r.map(|v| v / det))
}

#[cfg(test)]
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    if length == 0. {
        v
    } else {
        v.map(|x| x / length)
    }
}
#[cfg(test)]
pub(crate) fn illuminate(
    f: &State,
    lights: &[Option<[f32; 26]>; 8],
    eye: [f32; 4],
    normal: [f32; 3],
    matrix: &[[f32; 3]; 3],
    color1: Option<[f32; 4]>,
    color2: Option<[f32; 4]>,
) -> ([f32; 4], [f32; 4]) {
    let source = |state, offset| {
        if f.render[&Render::ColorVertex] != 0 {
            match f.render[&state] {
                1 if color1.is_some() => return color1.unwrap(),
                2 if color2.is_some() => return color2.unwrap(),
                _ => {}
            }
        }
        f.material.colors[offset]
    };
    let diffuse = source(Render::DiffuseSource, 0);
    let specular = source(Render::SpecularSource, 2);
    let ambient = source(Render::AmbientSource, 1);
    let emissive = source(Render::EmissiveSource, 3);
    let mut normal = std::array::from_fn(|c| (0..3).map(|r| normal[r] * matrix[r][c]).sum());
    if f.render[&Render::NormalizeNormals] != 0 {
        normal = normalize(normal);
    }
    let global = color(f.render[&Render::Ambient]);
    let mut result = std::array::from_fn(|n| emissive[n] + ambient[n] * global[n]);
    result[3] = diffuse[3];
    let mut highlight = [0.; 4];
    let viewer = if f.render[&Render::LocalViewer] != 0 {
        normalize([-eye[0], -eye[1], -eye[2]])
    } else {
        [0., 0., -1.]
    };
    for light in lights.iter().flatten() {
        let direction;
        let mut attenuation = 1.;
        if light[0] == 3. {
            let v = row([-light[16], -light[17], -light[18], 0.], &f.transforms[1]);
            direction = normalize([v[0], v[1], v[2]]);
        } else {
            let position = row([light[13], light[14], light[15], 1.], &f.transforms[1]);
            let delta = std::array::from_fn(|n| position[n] - eye[n]);
            let distance = dot(delta, delta).sqrt();
            if distance > light[19] {
                continue;
            }
            direction = normalize(delta);
            attenuation = 1. / (light[21] + light[22] * distance + light[23] * distance * distance);
            if light[0] == 2. {
                let v = row([light[16], light[17], light[18], 0.], &f.transforms[1]);
                let cosine = -dot(normalize([v[0], v[1], v[2]]), direction);
                let inner = (light[24] * 0.5).cos();
                let outer = (light[25] * 0.5).cos();
                attenuation *= if cosine > inner {
                    1.
                } else if cosine <= outer {
                    0.
                } else {
                    ((cosine - outer) / (inner - outer)).powf(light[20])
                };
            }
        }
        let ndotl = dot(normal, direction).max(0.);
        let power = if ndotl > 0. && f.render[&Render::SpecularEnable] != 0 {
            dot(
                normal,
                normalize(std::array::from_fn(|n| viewer[n] + direction[n])),
            )
            .max(0.)
            .powf(f.material.power)
        } else {
            0.
        };
        for n in 0..3 {
            result[n] +=
                attenuation * (ambient[n] * light[9 + n] + diffuse[n] * light[1 + n] * ndotl);
            highlight[n] += attenuation * specular[n] * light[5 + n] * power;
        }
    }
    (
        result.map(|v| v.clamp(0., 1.)),
        highlight.map(|v| v.clamp(0., 1.)),
    )
}
#[cfg(test)]
pub(crate) fn fog(f: &State, transformed: bool, z: f32, eye: [f32; 4], supplied: f32) -> f32 {
    let table = f.render[&Render::TableFog];
    let mode = if table != 0 {
        table
    } else if transformed {
        0
    } else {
        f.render[&Render::VertexFog]
    };
    let distance = if transformed {
        z
    } else if table == 0 && f.render[&Render::RangeFog] != 0 {
        dot([eye[0], eye[1], eye[2]], [eye[0], eye[1], eye[2]]).sqrt()
    } else {
        eye[2].abs()
    };
    let density = f32::from_bits(f.render[&Render::FogDensity]);
    match mode {
        1 => (-density * distance).exp(),
        2 => (-(density * distance).powi(2)).exp(),
        3 => ((f32::from_bits(f.render[&Render::FogEnd]) - distance)
            / (f32::from_bits(f.render[&Render::FogEnd])
                - f32::from_bits(f.render[&Render::FogStart])))
        .clamp(0., 1.),
        _ => supplied,
    }
}
