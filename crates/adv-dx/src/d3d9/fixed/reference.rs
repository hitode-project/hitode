//! Scalar reference for differential tests of the fixed vertex shader.
use super::*;

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    if length == 0. {
        v
    } else {
        v.map(|x| x / length)
    }
}
pub(in crate::d3d9::fixed) fn illuminate(
    f: &Fixed,
    lights: &[Option<[f32; 26]>; 8],
    eye: [f32; 4],
    normal: [f32; 3],
    matrix: &[[f32; 3]; 3],
    color1: Option<[f32; 4]>,
    color2: Option<[f32; 4]>,
) -> ([f32; 4], [f32; 4]) {
    let source = |state, offset| {
        if f.render[&141] != 0 {
            match f.render[&state] {
                1 if color1.is_some() => return color1.unwrap(),
                2 if color2.is_some() => return color2.unwrap(),
                _ => {}
            }
        }
        f.material[offset..offset + 4].try_into().unwrap()
    };
    let diffuse = source(145, 0);
    let specular = source(146, 8);
    let ambient = source(147, 4);
    let emissive = source(148, 12);
    let mut normal = std::array::from_fn(|c| (0..3).map(|r| normal[r] * matrix[r][c]).sum());
    if f.render[&143] != 0 {
        normal = normalize(normal);
    }
    let global = color(f.render[&139]);
    let mut result = std::array::from_fn(|n| emissive[n] + ambient[n] * global[n]);
    result[3] = diffuse[3];
    let mut highlight = [0.; 4];
    let viewer = if f.render[&142] != 0 {
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
        let power = if ndotl > 0. && f.render[&29] != 0 {
            dot(
                normal,
                normalize(std::array::from_fn(|n| viewer[n] + direction[n])),
            )
            .max(0.)
            .powf(f.material[16])
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
pub(in crate::d3d9::fixed) fn fog(
    f: &Fixed,
    transformed: bool,
    z: f32,
    eye: [f32; 4],
    supplied: f32,
) -> f32 {
    let table = f.render[&35];
    let mode = if table != 0 {
        table
    } else if transformed {
        0
    } else {
        f.render[&140]
    };
    let distance = if transformed {
        z
    } else if table == 0 && f.render[&48] != 0 {
        dot([eye[0], eye[1], eye[2]], [eye[0], eye[1], eye[2]]).sqrt()
    } else {
        eye[2].abs()
    };
    let density = f32::from_bits(f.render[&38]);
    match mode {
        1 => (-density * distance).exp(),
        2 => (-(density * distance).powi(2)).exp(),
        3 => ((f32::from_bits(f.render[&37]) - distance)
            / (f32::from_bits(f.render[&37]) - f32::from_bits(f.render[&36])))
        .clamp(0., 1.),
        _ => supplied,
    }
}
