//! Native direct bodies for Wine 11 D3DX9 math functions (A64F-FP-10, NAT-BODY-18).
//!
//! All bound exports are confirmed across `d3dx9_24.dll` through `d3dx9_43.dll`
//! identity tables, use `stdcall`, return pointer/`HRESULT`/`void` in `EAX`, and
//! stay within 2 ULP (scalar) and 4 ULP (compound) of binary32 with exact special
//! values, aliasing safety, and checked memory accesses (A64-CALL-4, NAT-BODY-17).

use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use adv_machine::Conv;

use super::{Body, resume_original};

const D3DERR_INVALIDCALL: u32 = 0x8876_086c;

const IN2: &[Input] = &[Input::Stack(0), Input::Stack(1)];
const IN3: &[Input] = &[Input::Stack(0), Input::Stack(1), Input::Stack(2)];
const IN4: &[Input] = &[
    Input::Stack(0),
    Input::Stack(1),
    Input::Stack(2),
    Input::Stack(3),
];
const IN5: &[Input] = &[
    Input::Stack(0),
    Input::Stack(1),
    Input::Stack(2),
    Input::Stack(3),
    Input::Stack(4),
];
const IN6: &[Input] = &[
    Input::Stack(0),
    Input::Stack(1),
    Input::Stack(2),
    Input::Stack(3),
    Input::Stack(4),
    Input::Stack(5),
];
const IN7: &[Input] = &[
    Input::Stack(0),
    Input::Stack(1),
    Input::Stack(2),
    Input::Stack(3),
    Input::Stack(4),
    Input::Stack(5),
    Input::Stack(6),
];

type Vec2 = [f32; 2];
type Vec3 = [f32; 3];
type Vec4 = [f32; 4];
type Quat = [f32; 4];
type Plane = [f32; 4];
type Mat4 = [f32; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

#[inline]
fn f32_arg(bits: u32) -> f32 {
    f32::from_bits(bits)
}

#[inline]
unsafe fn load_f32s<const N: usize>(ptr: *const u8) -> [f32; N] {
    let mut out = [0.0f32; N];
    let mut i = 0;
    while i < N {
        // SAFETY: caller verified `ptr` spans `N * 4` readable bytes.
        let word = u32::from_le(unsafe { ptr.add(i * 4).cast::<u32>().read_unaligned() });
        out[i] = f32::from_bits(word);
        i += 1;
    }
    out
}

#[inline]
unsafe fn store_f32s<const N: usize>(ptr: *mut u8, vals: &[f32; N]) {
    let mut i = 0;
    while i < N {
        // SAFETY: caller verified `ptr` spans `N * 4` writable bytes.
        unsafe {
            ptr.add(i * 4)
                .cast::<u32>()
                .write_unaligned(vals[i].to_bits().to_le());
        }
        i += 1;
    }
}

#[inline]
fn read_f32s<M: Mem, const N: usize>(mem: &mut M, at: u32) -> Result<[f32; N], Stop> {
    let ptr = mem.input(at, (N * 4) as u32)?;
    // SAFETY: `mem.input` verified `N * 4` readable bytes.
    Ok(unsafe { load_f32s::<N>(ptr) })
}

#[inline]
fn write_f32s<M: Mem, const N: usize>(mem: &mut M, at: u32, vals: &[f32; N]) -> Result<(), Stop> {
    let ptr = mem.output(at, (N * 4) as u32)?;
    // SAFETY: `mem.output` verified `N * 4` writable bytes.
    unsafe { store_f32s::<N>(ptr, vals) };
    mem.commit()
}

struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_z: f32,
    max_z: f32,
}

fn read_viewport<M: Mem>(mem: &mut M, at: u32) -> Result<Viewport, Stop> {
    let ptr = mem.input(at, 24)?;
    // SAFETY: `mem.input` verified 24 readable bytes.
    let raw = unsafe {
        std::array::from_fn::<u32, 6, _>(|i| {
            u32::from_le(ptr.add(i * 4).cast::<u32>().read_unaligned())
        })
    };
    Ok(Viewport {
        x: raw[0] as f32,
        y: raw[1] as f32,
        width: raw[2] as f32,
        height: raw[3] as f32,
        min_z: f32::from_bits(raw[4]),
        max_z: f32::from_bits(raw[5]),
    })
}

// ============================================================================
// Pure binary32 math kernels (no FMA contraction, exact special values)
// ============================================================================

#[inline]
fn vec3_dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn vec3_cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn vec3_normalize_val(v: Vec3) -> Vec3 {
    let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if norm == 0.0 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / norm, v[1] / norm, v[2] / norm]
    }
}

#[inline]
fn plane_normalize_val(p: Plane) -> Plane {
    let norm = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    if norm == 0.0 {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        [p[0] / norm, p[1] / norm, p[2] / norm, p[3] / norm]
    }
}

#[inline]
fn mat_mul_val(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0f32; 16];
    let mut i = 0;
    while i < 4 {
        let r = i * 4;
        let a0 = a[r];
        let a1 = a[r + 1];
        let a2 = a[r + 2];
        let a3 = a[r + 3];
        let mut j = 0;
        while j < 4 {
            out[r + j] = a0 * b[j] + a1 * b[4 + j] + a2 * b[8 + j] + a3 * b[12 + j];
            j += 1;
        }
        i += 1;
    }
    out
}

#[inline]
fn mat_mul_transpose_val(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0f32; 16];
    let mut i = 0;
    while i < 4 {
        let r = i * 4;
        let a0 = a[r];
        let a1 = a[r + 1];
        let a2 = a[r + 2];
        let a3 = a[r + 3];
        let mut j = 0;
        while j < 4 {
            out[j * 4 + i] = a0 * b[j] + a1 * b[4 + j] + a2 * b[8 + j] + a3 * b[12 + j];
            j += 1;
        }
        i += 1;
    }
    out
}

fn mat_inverse_val(m: &Mat4) -> Option<(Mat4, f32)> {
    let t0 = m[10] * m[15] - m[11] * m[14];
    let t1 = m[6] * m[15] - m[7] * m[14];
    let t2 = m[6] * m[11] - m[7] * m[10];
    let v0 = m[5] * t0 - m[9] * t1 + m[13] * t2;
    let v4 = -m[4] * t0 + m[8] * t1 - m[12] * t2;

    let t0 = m[4] * m[9] - m[8] * m[5];
    let t1 = m[4] * m[13] - m[12] * m[5];
    let t2 = m[8] * m[13] - m[12] * m[9];
    let v8 = m[15] * t0 - m[11] * t1 + m[7] * t2;
    let v12 = -m[14] * t0 + m[10] * t1 - m[6] * t2;

    let det = m[0] * v0 + m[1] * v4 + m[2] * v8 + m[3] * v12;
    if det == 0.0 {
        return None;
    }

    let t0 = m[10] * m[15] - m[11] * m[14];
    let t1 = m[2] * m[15] - m[3] * m[14];
    let t2 = m[2] * m[11] - m[3] * m[10];
    let v1 = -m[1] * t0 + m[9] * t1 - m[13] * t2;
    let v5 = m[0] * t0 - m[8] * t1 + m[12] * t2;

    let t0 = m[0] * m[9] - m[8] * m[1];
    let t1 = m[12] * m[1] - m[0] * m[13];
    let t2 = m[8] * m[13] - m[12] * m[9];
    let v9 = -m[15] * t0 - m[11] * t1 - m[3] * t2;
    let v13 = m[14] * t0 + m[10] * t1 + m[2] * t2;

    let t0 = m[6] * m[15] - m[7] * m[14];
    let t1 = m[2] * m[15] - m[3] * m[14];
    let t2 = m[2] * m[7] - m[3] * m[6];
    let v2 = m[1] * t0 - m[5] * t1 + m[13] * t2;
    let v6 = -m[0] * t0 + m[4] * t1 - m[12] * t2;

    let t0 = m[0] * m[5] - m[4] * m[1];
    let t1 = m[12] * m[1] - m[0] * m[13];
    let t2 = m[4] * m[13] - m[12] * m[5];
    let v10 = m[15] * t0 + m[7] * t1 + m[3] * t2;
    let v14 = -m[14] * t0 - m[6] * t1 - m[2] * t2;

    let t0 = m[6] * m[11] - m[7] * m[10];
    let t1 = m[2] * m[11] - m[3] * m[10];
    let t2 = m[2] * m[7] - m[3] * m[6];
    let v3 = -m[1] * t0 + m[5] * t1 - m[9] * t2;
    let v7 = m[0] * t0 - m[4] * t1 + m[8] * t2;

    let v11 = -m[0] * (m[5] * m[11] - m[7] * m[9]) + m[4] * (m[1] * m[11] - m[3] * m[9])
        - m[8] * (m[1] * m[7] - m[3] * m[5]);
    let v15 = m[0] * (m[5] * m[10] - m[6] * m[9]) - m[4] * (m[1] * m[10] - m[2] * m[9])
        + m[8] * (m[1] * m[6] - m[2] * m[5]);

    let inv = 1.0f32 / det;
    let v = [
        v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15,
    ];
    let out = std::array::from_fn(|i| v[i] * inv);
    Some((out, det))
}

#[inline]
fn mat_rotation_quat_val(q: Quat) -> Mat4 {
    let [x, y, z, w] = q;
    let mut out = IDENTITY;
    out[0] = 1.0 - 2.0 * (y * y + z * z);
    out[1] = 2.0 * (x * y + z * w);
    out[2] = 2.0 * (x * z - y * w);
    out[4] = 2.0 * (x * y - z * w);
    out[5] = 1.0 - 2.0 * (x * x + z * z);
    out[6] = 2.0 * (y * z + x * w);
    out[8] = 2.0 * (x * z + y * w);
    out[9] = 2.0 * (y * z - x * w);
    out[10] = 1.0 - 2.0 * (x * x + y * y);
    out
}

#[inline]
fn mat_scaling_val(sx: f32, sy: f32, sz: f32) -> Mat4 {
    let mut out = IDENTITY;
    out[0] = sx;
    out[5] = sy;
    out[10] = sz;
    out
}

#[inline]
fn mat_translation_val(x: f32, y: f32, z: f32) -> Mat4 {
    let mut out = IDENTITY;
    out[12] = x;
    out[13] = y;
    out[14] = z;
    out
}

fn mat_transformation_val(
    sc: Option<Vec3>,
    sr: Option<Quat>,
    s: Option<Vec3>,
    rc: Option<Vec3>,
    r: Option<Quat>,
    t: Option<Vec3>,
) -> Mat4 {
    let mut m1 = if let Some(s) = s {
        let sc = sc.unwrap_or([0.0, 0.0, 0.0]);
        let mut m = mat_translation_val(-sc[0], -sc[1], -sc[2]);
        if let Some(sr) = sr {
            let msr1 = mat_rotation_quat_val([-sr[0], -sr[1], -sr[2], sr[3]]);
            m = mat_mul_val(&m, &msr1);
        }
        let ms = mat_scaling_val(s[0], s[1], s[2]);
        m = mat_mul_val(&m, &ms);
        if let Some(sr) = sr {
            let msr = mat_rotation_quat_val(sr);
            m = mat_mul_val(&m, &msr);
        }
        let msc = mat_translation_val(sc[0], sc[1], sc[2]);
        mat_mul_val(&m, &msc)
    } else {
        IDENTITY
    };
    if let Some(r) = r {
        let rc = rc.unwrap_or([0.0, 0.0, 0.0]);
        let mrc1 = mat_translation_val(-rc[0], -rc[1], -rc[2]);
        m1 = mat_mul_val(&m1, &mrc1);
        let mr = mat_rotation_quat_val(r);
        m1 = mat_mul_val(&m1, &mr);
        let mrc = mat_translation_val(rc[0], rc[1], rc[2]);
        m1 = mat_mul_val(&m1, &mrc);
    }
    if let Some(t) = t {
        let mt = mat_translation_val(t[0], t[1], t[2]);
        mat_mul_val(&m1, &mt)
    } else {
        m1
    }
}

#[inline]
fn quat_mul_val(q1: Quat, q2: Quat) -> Quat {
    [
        q2[3] * q1[0] + q2[0] * q1[3] + q2[1] * q1[2] - q2[2] * q1[1],
        q2[3] * q1[1] - q2[0] * q1[2] + q2[1] * q1[3] + q2[2] * q1[0],
        q2[3] * q1[2] + q2[0] * q1[1] - q2[1] * q1[0] + q2[2] * q1[3],
        q2[3] * q1[3] - q2[0] * q1[0] - q2[1] * q1[1] - q2[2] * q1[2],
    ]
}

#[inline]
fn quat_dot(a: Quat, b: Quat) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

#[inline]
fn quat_inverse_val(q: Quat) -> Quat {
    let norm = quat_dot(q, q);
    [-q[0] / norm, -q[1] / norm, -q[2] / norm, q[3] / norm]
}

#[inline]
fn quat_exp_val(q: Quat) -> Quat {
    let norm = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
    if norm != 0.0 {
        let s = norm.sin();
        [
            s * q[0] / norm,
            s * q[1] / norm,
            s * q[2] / norm,
            norm.cos(),
        ]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

#[inline]
fn quat_ln_val(q: Quat) -> Quat {
    let t = if q[3] >= 1.0 || q[3] == -1.0 {
        1.0
    } else {
        q[3].acos() / (1.0 - q[3] * q[3]).sqrt()
    };
    [t * q[0], t * q[1], t * q[2], 0.0]
}

fn quat_slerp_val(q1: Quat, q2: Quat, mut t: f32) -> Quat {
    let mut temp = 1.0 - t;
    let mut dot = quat_dot(q1, q2);
    if dot < 0.0 {
        t = -t;
        dot = -dot;
    }
    if 1.0 - dot > 0.001 {
        let theta = dot.acos();
        let sin_theta = theta.sin();
        temp = (theta * temp).sin() / sin_theta;
        t = (theta * t).sin() / sin_theta;
    }
    [
        temp * q1[0] + t * q2[0],
        temp * q1[1] + t * q2[1],
        temp * q1[2] + t * q2[2],
        temp * q1[3] + t * q2[3],
    ]
}

fn quat_rotation_matrix_val(m: &Mat4) -> Quat {
    let trace = m[0] + m[5] + m[10] + 1.0;
    if trace > 1.0 {
        let s = 2.0 * trace.sqrt();
        [
            (m[6] - m[9]) / s,
            (m[8] - m[2]) / s,
            (m[1] - m[4]) / s,
            0.25 * s,
        ]
    } else {
        let mut maxi = 0;
        if m[5] > m[0] {
            maxi = 1;
        }
        if m[10] > m[maxi * 5] {
            maxi = 2;
        }
        match maxi {
            0 => {
                let s = 2.0 * (1.0 + m[0] - m[5] - m[10]).sqrt();
                [
                    0.25 * s,
                    (m[1] + m[4]) / s,
                    (m[2] + m[8]) / s,
                    (m[6] - m[9]) / s,
                ]
            }
            1 => {
                let s = 2.0 * (1.0 + m[5] - m[0] - m[10]).sqrt();
                [
                    (m[1] + m[4]) / s,
                    0.25 * s,
                    (m[6] + m[9]) / s,
                    (m[8] - m[2]) / s,
                ]
            }
            _ => {
                let s = 2.0 * (1.0 + m[10] - m[0] - m[5]).sqrt();
                [
                    (m[2] + m[8]) / s,
                    (m[6] + m[9]) / s,
                    0.25 * s,
                    (m[1] - m[4]) / s,
                ]
            }
        }
    }
}

#[inline]
fn vec2_transform_val(v: Vec2, m: &Mat4) -> Vec4 {
    [
        m[0] * v[0] + m[4] * v[1] + m[12],
        m[1] * v[0] + m[5] * v[1] + m[13],
        m[2] * v[0] + m[6] * v[1] + m[14],
        m[3] * v[0] + m[7] * v[1] + m[15],
    ]
}

#[inline]
fn vec2_transform_coord_val(v: Vec2, m: &Mat4) -> Vec2 {
    let norm = m[3] * v[0] + m[7] * v[1] + m[15];
    [
        (m[0] * v[0] + m[4] * v[1] + m[12]) / norm,
        (m[1] * v[0] + m[5] * v[1] + m[13]) / norm,
    ]
}

#[inline]
fn vec2_transform_normal_val(v: Vec2, m: &Mat4) -> Vec2 {
    [m[0] * v[0] + m[4] * v[1], m[1] * v[0] + m[5] * v[1]]
}

#[inline]
fn vec3_transform_val(v: Vec3, m: &Mat4) -> Vec4 {
    [
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14],
        m[3] * v[0] + m[7] * v[1] + m[11] * v[2] + m[15],
    ]
}

#[inline]
fn vec3_transform_coord_val(v: Vec3, m: &Mat4) -> Vec3 {
    let norm = m[3] * v[0] + m[7] * v[1] + m[11] * v[2] + m[15];
    [
        (m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12]) / norm,
        (m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13]) / norm,
        (m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14]) / norm,
    ]
}

#[inline]
fn vec3_transform_normal_val(v: Vec3, m: &Mat4) -> Vec3 {
    [
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2],
    ]
}

#[inline]
fn vec4_transform_val(v: Vec4, m: &Mat4) -> Vec4 {
    [
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12] * v[3],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13] * v[3],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14] * v[3],
        m[3] * v[0] + m[7] * v[1] + m[11] * v[2] + m[15] * v[3],
    ]
}

#[inline]
fn plane_transform_val(p: Plane, m: &Mat4) -> Plane {
    vec4_transform_val(p, m)
}

/// Shared checked array transform helper: validates all touched input/output
/// elements before any guest write so faults never leave partial mutations
/// (A64-CALL-4, NAT-BODY-16).
fn transform_array<M: Mem, const IN_N: usize, const OUT_N: usize>(
    mem: &mut M,
    [out, outstride, inp, instride, matrix, elements, ..]: [u32; 7],
    f: impl Fn([f32; IN_N], &Mat4) -> [f32; OUT_N],
) -> Result<Ret, Stop> {
    if elements == 0 {
        return Ok(out.into());
    }
    let m = read_f32s::<M, 16>(mem, matrix)?;
    let in_bytes = (IN_N * 4) as u32;
    let out_bytes = (OUT_N * 4) as u32;

    // Fast path: contiguous, non-wrapping spans. One span check covers all pages
    // and preserves aliasing identically in Window and GuestMem.
    if instride == in_bytes
        && outstride == out_bytes
        && let (Some(in_total), Some(out_total)) = (
            elements.checked_mul(in_bytes),
            elements.checked_mul(out_bytes),
        )
        && inp.checked_add(in_total).is_some()
        && out.checked_add(out_total).is_some()
    {
        let [src, dst] = mem.spans([(inp, in_total, false), (out, out_total, true)])?;
        for i in 0..elements as usize {
            // SAFETY: both contiguous spans are verified and share live aliasing.
            let v = unsafe { load_f32s::<IN_N>(src.add(i * IN_N * 4)) };
            let r = f(v, &m);
            unsafe { store_f32s::<OUT_N>(dst.add(i * OUT_N * 4), &r) };
        }
        mem.commit()?;
        return Ok(out.into());
    }

    // Zero-stride special case: single input and output element checked once.
    if instride == 0 && outstride == 0 {
        let [src, dst] = mem.spans([(inp, in_bytes, false), (out, out_bytes, true)])?;
        for _ in 0..elements {
            // SAFETY: both element spans are verified and share live aliasing.
            let v = unsafe { load_f32s::<IN_N>(src) };
            let r = f(v, &m);
            unsafe { store_f32s::<OUT_N>(dst, &r) };
        }
        mem.commit()?;
        return Ok(out.into());
    }

    // General strided path: check all accessed pages before any write.
    // Track last checked page to avoid redundant class-table reads (A64-CALL-4).
    let mut last_in_page = u32::MAX;
    let mut last_out_page = u32::MAX;
    for i in 0..elements {
        let a = inp.wrapping_add(instride.wrapping_mul(i));
        let b = out.wrapping_add(outstride.wrapping_mul(i));
        let a_end = a.wrapping_add(in_bytes - 1);
        if (a >> 12) != last_in_page || (a_end >> 12) != (a >> 12) || a_end < a {
            mem.input(a, in_bytes)?;
            last_in_page = a >> 12;
        }
        let b_end = b.wrapping_add(out_bytes - 1);
        if (b >> 12) != last_out_page || (b_end >> 12) != (b >> 12) || b_end < b {
            mem.output(b, out_bytes)?;
            last_out_page = b >> 12;
        }
    }
    for i in 0..elements {
        let a = inp.wrapping_add(instride.wrapping_mul(i));
        let b = out.wrapping_add(outstride.wrapping_mul(i));
        let [src, dst] = mem.spans([(a, in_bytes, false), (b, out_bytes, true)])?;
        // SAFETY: element spans are verified and share live aliasing.
        let v = unsafe { load_f32s::<IN_N>(src) };
        let r = f(v, &m);
        unsafe { store_f32s::<OUT_N>(dst, &r) };
        mem.commit()?;
    }
    Ok(out.into())
}

// ============================================================================
// DirectBody implementations
// ============================================================================

fn color_adjust_contrast<M: Mem>(mem: &mut M, [pout, pc, s, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let c = read_f32s::<M, 4>(mem, pc)?;
    let s = f32_arg(s);
    let out = [
        0.5 + s * (c[0] - 0.5),
        0.5 + s * (c[1] - 0.5),
        0.5 + s * (c[2] - 0.5),
        c[3],
    ];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn color_adjust_saturation<M: Mem>(mem: &mut M, [pout, pc, s, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let c = read_f32s::<M, 4>(mem, pc)?;
    let s = f32_arg(s);
    let grey = c[0] * 0.2125 + c[1] * 0.7154 + c[2] * 0.0721;
    let out = [
        grey + s * (c[0] - grey),
        grey + s * (c[1] - grey),
        grey + s * (c[2] - grey),
        c[3],
    ];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_affine_transformation<M: Mem>(
    mem: &mut M,
    [out_ptr, scaling, rc_ptr, rot_ptr, trans_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let scaling = f32_arg(scaling);
    let rot = if rot_ptr != 0 {
        Some(read_f32s::<M, 4>(mem, rot_ptr)?)
    } else {
        None
    };
    let rc = if rot.is_some() && rc_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, rc_ptr)?)
    } else {
        None
    };
    let trans = if trans_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, trans_ptr)?)
    } else {
        None
    };
    let mut out = IDENTITY;
    if let Some([x, y, z, w]) = rot {
        let t00 = 1.0 - 2.0 * (y * y + z * z);
        let t01 = 2.0 * (x * y + z * w);
        let t02 = 2.0 * (x * z - y * w);
        let t10 = 2.0 * (x * y - z * w);
        let t11 = 1.0 - 2.0 * (x * x + z * z);
        let t12 = 2.0 * (y * z + x * w);
        let t20 = 2.0 * (x * z + y * w);
        let t21 = 2.0 * (y * z - x * w);
        let t22 = 1.0 - 2.0 * (x * x + y * y);
        out[0] = scaling * t00;
        out[1] = scaling * t01;
        out[2] = scaling * t02;
        out[4] = scaling * t10;
        out[5] = scaling * t11;
        out[6] = scaling * t12;
        out[8] = scaling * t20;
        out[9] = scaling * t21;
        out[10] = scaling * t22;
        if let Some([cx, cy, cz]) = rc {
            out[12] = cx * (1.0 - t00) - cy * t10 - cz * t20;
            out[13] = cy * (1.0 - t11) - cx * t01 - cz * t21;
            out[14] = cz * (1.0 - t22) - cx * t02 - cy * t12;
        }
    } else {
        out[0] = scaling;
        out[5] = scaling;
        out[10] = scaling;
    }
    if let Some([tx, ty, tz]) = trans {
        out[12] += tx;
        out[13] += ty;
        out[14] += tz;
    }
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_affine_transformation_2d<M: Mem>(
    mem: &mut M,
    [out_ptr, scaling, rc_ptr, rotation, trans_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let scaling = f32_arg(scaling);
    let rotation = f32_arg(rotation);
    let rc = if rc_ptr != 0 {
        Some(read_f32s::<M, 2>(mem, rc_ptr)?)
    } else {
        None
    };
    let trans = if trans_ptr != 0 {
        Some(read_f32s::<M, 2>(mem, trans_ptr)?)
    } else {
        None
    };
    let half = rotation / 2.0;
    let s = half.sin();
    let tmp1 = 1.0 - 2.0 * s * s;
    let tmp2 = 2.0 * s * half.cos();
    let mut out = IDENTITY;
    out[0] = scaling * tmp1;
    out[1] = scaling * tmp2;
    out[4] = -scaling * tmp2;
    out[5] = scaling * tmp1;
    if let Some([x, y]) = rc {
        out[12] = y * tmp2 - x * tmp1 + x;
        out[13] = -x * tmp2 - y * tmp1 + y;
    }
    if let Some([tx, ty]) = trans {
        out[12] += tx;
        out[13] += ty;
    }
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_decompose<M: Mem>(
    mem: &mut M,
    [scale_ptr, rot_ptr, trans_ptr, pm, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let m = read_f32s::<M, 16>(mem, pm)?;
    let sx = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
    let sy = (m[4] * m[4] + m[5] * m[5] + m[6] * m[6]).sqrt();
    let sz = (m[8] * m[8] + m[9] * m[9] + m[10] * m[10]).sqrt();
    let scale = [sx, sy, sz];
    let trans = [m[12], m[13], m[14]];
    if sx == 0.0 || sy == 0.0 || sz == 0.0 {
        let [ps, pt] = mem.spans([(scale_ptr, 12, true), (trans_ptr, 12, true)])?;
        // SAFETY: both spans verified and share live aliasing.
        unsafe {
            store_f32s(ps, &scale);
            store_f32s(pt, &trans);
        }
        mem.commit()?;
        return Ok(D3DERR_INVALIDCALL.into());
    }
    let mut normalized = [0.0f32; 16];
    normalized[0] = m[0] / sx;
    normalized[1] = m[1] / sx;
    normalized[2] = m[2] / sx;
    normalized[4] = m[4] / sy;
    normalized[5] = m[5] / sy;
    normalized[6] = m[6] / sy;
    normalized[8] = m[8] / sz;
    normalized[9] = m[9] / sz;
    normalized[10] = m[10] / sz;
    let rot = quat_rotation_matrix_val(&normalized);
    let [ps, pt, pr] = mem.spans([
        (scale_ptr, 12, true),
        (trans_ptr, 12, true),
        (rot_ptr, 16, true),
    ])?;
    // SAFETY: all three spans verified and share live aliasing.
    unsafe {
        store_f32s(ps, &scale);
        store_f32s(pt, &trans);
        store_f32s(pr, &rot);
    }
    mem.commit()?;
    Ok(0u32.into())
}

fn matrix_inverse<M: Mem>(mem: &mut M, [pout, pdet, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let m = read_f32s::<M, 16>(mem, pm)?;
    let Some((inv, det)) = mat_inverse_val(&m) else {
        return Ok(0u32.into());
    };
    if pdet != 0 {
        let [pd, po] = mem.spans([(pdet, 4, true), (pout, 64, true)])?;
        // SAFETY: both spans verified and share live aliasing.
        unsafe {
            store_f32s(pd, &[det]);
            store_f32s(po, &inv);
        }
        mem.commit()?;
    } else {
        write_f32s(mem, pout, &inv)?;
    }
    Ok(pout.into())
}

fn matrix_look_at<M: Mem, const RH: bool>(
    mem: &mut M,
    [out_ptr, eye_ptr, at_ptr, up_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let eye = read_f32s::<M, 3>(mem, eye_ptr)?;
    let at = read_f32s::<M, 3>(mem, at_ptr)?;
    let up = read_f32s::<M, 3>(mem, up_ptr)?;
    let vec = vec3_normalize_val([at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]]);
    let right = vec3_cross(up, vec);
    let upn = vec3_normalize_val(vec3_cross(vec, right));
    let right = vec3_normalize_val(right);
    let out = if RH {
        [
            -right[0],
            upn[0],
            -vec[0],
            0.0,
            -right[1],
            upn[1],
            -vec[1],
            0.0,
            -right[2],
            upn[2],
            -vec[2],
            0.0,
            vec3_dot(right, eye),
            -vec3_dot(upn, eye),
            vec3_dot(vec, eye),
            1.0,
        ]
    } else {
        [
            right[0],
            upn[0],
            vec[0],
            0.0,
            right[1],
            upn[1],
            vec[1],
            0.0,
            right[2],
            upn[2],
            vec[2],
            0.0,
            -vec3_dot(right, eye),
            -vec3_dot(upn, eye),
            -vec3_dot(vec, eye),
            1.0,
        ]
    };
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_multiply<M: Mem>(mem: &mut M, [pout, pm1, pm2, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let m1 = read_f32s::<M, 16>(mem, pm1)?;
    let m2 = read_f32s::<M, 16>(mem, pm2)?;
    let out = mat_mul_val(&m1, &m2);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_multiply_transpose<M: Mem>(
    mem: &mut M,
    [pout, pm1, pm2, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let m1 = read_f32s::<M, 16>(mem, pm1)?;
    let m2 = read_f32s::<M, 16>(mem, pm2)?;
    let out = mat_mul_transpose_val(&m1, &m2);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_ortho<M: Mem, const RH: bool>(
    mem: &mut M,
    [pout, w, h, zn, zf, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let (w, h, zn, zf) = (f32_arg(w), f32_arg(h), f32_arg(zn), f32_arg(zf));
    let mut out = IDENTITY;
    out[0] = 2.0 / w;
    out[5] = 2.0 / h;
    out[10] = if RH { 1.0 / (zn - zf) } else { 1.0 / (zf - zn) };
    out[14] = zn / (zn - zf);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_ortho_off_center<M: Mem, const RH: bool>(
    mem: &mut M,
    [pout, l, r, b, t, zn, zf]: [u32; 7],
) -> Result<Ret, Stop> {
    let (l, r, b, t, zn, zf) = (
        f32_arg(l),
        f32_arg(r),
        f32_arg(b),
        f32_arg(t),
        f32_arg(zn),
        f32_arg(zf),
    );
    let mut out = IDENTITY;
    out[0] = 2.0 / (r - l);
    out[5] = 2.0 / (t - b);
    out[10] = if RH { 1.0 / (zn - zf) } else { 1.0 / (zf - zn) };
    out[12] = -1.0 - 2.0 * l / (r - l);
    out[13] = 1.0 + 2.0 * t / (b - t);
    out[14] = zn / (zn - zf);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_perspective_fov<M: Mem, const RH: bool>(
    mem: &mut M,
    [pout, fovy, aspect, zn, zf, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let (fovy, aspect, zn, zf) = (f32_arg(fovy), f32_arg(aspect), f32_arg(zn), f32_arg(zf));
    let tan_half = (fovy / 2.0).tan();
    let mut out = IDENTITY;
    out[0] = 1.0 / (aspect * tan_half);
    out[5] = 1.0 / tan_half;
    out[10] = if RH { zf / (zn - zf) } else { zf / (zf - zn) };
    out[11] = if RH { -1.0 } else { 1.0 };
    out[14] = (zf * zn) / (zn - zf);
    out[15] = 0.0;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_perspective<M: Mem, const RH: bool>(
    mem: &mut M,
    [pout, w, h, zn, zf, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let (w, h, zn, zf) = (f32_arg(w), f32_arg(h), f32_arg(zn), f32_arg(zf));
    let mut out = IDENTITY;
    out[0] = 2.0 * zn / w;
    out[5] = 2.0 * zn / h;
    out[10] = if RH { zf / (zn - zf) } else { zf / (zf - zn) };
    out[11] = if RH { -1.0 } else { 1.0 };
    out[14] = (zn * zf) / (zn - zf);
    out[15] = 0.0;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_perspective_off_center<M: Mem, const RH: bool>(
    mem: &mut M,
    [pout, l, r, b, t, zn, zf]: [u32; 7],
) -> Result<Ret, Stop> {
    let (l, r, b, t, zn, zf) = (
        f32_arg(l),
        f32_arg(r),
        f32_arg(b),
        f32_arg(t),
        f32_arg(zn),
        f32_arg(zf),
    );
    let mut out = IDENTITY;
    out[0] = 2.0 * zn / (r - l);
    out[5] = -2.0 * zn / (b - t);
    if RH {
        out[8] = 1.0 + 2.0 * l / (r - l);
        out[9] = -1.0 - 2.0 * t / (b - t);
        out[10] = zf / (zn - zf);
        out[11] = -1.0;
    } else {
        out[8] = -1.0 - 2.0 * l / (r - l);
        out[9] = 1.0 + 2.0 * t / (b - t);
        out[10] = -zf / (zn - zf);
        out[11] = 1.0;
    }
    out[14] = (zn * zf) / (zn - zf);
    out[15] = 0.0;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_reflect<M: Mem>(mem: &mut M, [pout, pplane, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let [a, b, c, d] = plane_normalize_val(read_f32s::<M, 4>(mem, pplane)?);
    let mut out = IDENTITY;
    out[0] = 1.0 - 2.0 * a * a;
    out[1] = -2.0 * a * b;
    out[2] = -2.0 * a * c;
    out[4] = -2.0 * a * b;
    out[5] = 1.0 - 2.0 * b * b;
    out[6] = -2.0 * b * c;
    out[8] = -2.0 * c * a;
    out[9] = -2.0 * c * b;
    out[10] = 1.0 - 2.0 * c * c;
    out[12] = -2.0 * d * a;
    out[13] = -2.0 * d * b;
    out[14] = -2.0 * d * c;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_rotation_axis<M: Mem>(
    mem: &mut M,
    [out_ptr, v_ptr, angle, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let nv = vec3_normalize_val(read_f32s::<M, 3>(mem, v_ptr)?);
    let (sangle, cangle) = f32_arg(angle).sin_cos();
    let cdiff = 1.0 - cangle;
    let out = [
        cdiff * nv[0] * nv[0] + cangle,
        cdiff * nv[1] * nv[0] + sangle * nv[2],
        cdiff * nv[2] * nv[0] - sangle * nv[1],
        0.0,
        cdiff * nv[0] * nv[1] - sangle * nv[2],
        cdiff * nv[1] * nv[1] + cangle,
        cdiff * nv[2] * nv[1] + sangle * nv[0],
        0.0,
        cdiff * nv[0] * nv[2] + sangle * nv[1],
        cdiff * nv[1] * nv[2] - sangle * nv[0],
        cdiff * nv[2] * nv[2] + cangle,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_rotation_quaternion<M: Mem>(mem: &mut M, [pout, pq, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q = read_f32s::<M, 4>(mem, pq)?;
    let out = mat_rotation_quat_val(q);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_rotation_x<M: Mem>(mem: &mut M, [pout, angle, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let (s, c) = f32_arg(angle).sin_cos();
    let mut out = IDENTITY;
    out[5] = c;
    out[10] = c;
    out[6] = s;
    out[9] = -s;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_rotation_y<M: Mem>(mem: &mut M, [pout, angle, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let (s, c) = f32_arg(angle).sin_cos();
    let mut out = IDENTITY;
    out[0] = c;
    out[10] = c;
    out[2] = -s;
    out[8] = s;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_rotation_z<M: Mem>(mem: &mut M, [pout, angle, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let (s, c) = f32_arg(angle).sin_cos();
    let mut out = IDENTITY;
    out[0] = c;
    out[5] = c;
    out[1] = s;
    out[4] = -s;
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_rotation_yaw_pitch_roll<M: Mem>(
    mem: &mut M,
    [out_ptr, yaw, pitch, roll, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let (syaw, cyaw) = f32_arg(yaw).sin_cos();
    let (spitch, cpitch) = f32_arg(pitch).sin_cos();
    let (sroll, croll) = f32_arg(roll).sin_cos();
    let out = [
        sroll * spitch * syaw + croll * cyaw,
        sroll * cpitch,
        sroll * spitch * cyaw - croll * syaw,
        0.0,
        croll * spitch * syaw - sroll * cyaw,
        croll * cpitch,
        croll * spitch * cyaw + sroll * syaw,
        0.0,
        cpitch * syaw,
        -spitch,
        cpitch * cyaw,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_scaling<M: Mem>(mem: &mut M, [pout, sx, sy, sz, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let out = mat_scaling_val(f32_arg(sx), f32_arg(sy), f32_arg(sz));
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_shadow<M: Mem>(mem: &mut M, [pout, plight, pplane, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let [a, b, c, d] = plane_normalize_val(read_f32s::<M, 4>(mem, pplane)?);
    let [lx, ly, lz, lw] = read_f32s::<M, 4>(mem, plight)?;
    let dot = a * lx + b * ly + c * lz + d * lw;
    let out = [
        dot - a * lx,
        -a * ly,
        -a * lz,
        -a * lw,
        -b * lx,
        dot - b * ly,
        -b * lz,
        -b * lw,
        -c * lx,
        -c * ly,
        dot - c * lz,
        -c * lw,
        -d * lx,
        -d * ly,
        -d * lz,
        dot - d * lw,
    ];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_transformation<M: Mem>(
    mem: &mut M,
    [out_ptr, sc_ptr, sr_ptr, s_ptr, rc_ptr, r_ptr, t_ptr]: [u32; 7],
) -> Result<Ret, Stop> {
    let s = if s_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, s_ptr)?)
    } else {
        None
    };
    let sc = if s.is_some() && sc_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, sc_ptr)?)
    } else {
        None
    };
    let sr = if s.is_some() && sr_ptr != 0 {
        Some(read_f32s::<M, 4>(mem, sr_ptr)?)
    } else {
        None
    };
    let r = if r_ptr != 0 {
        Some(read_f32s::<M, 4>(mem, r_ptr)?)
    } else {
        None
    };
    let rc = if r.is_some() && rc_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, rc_ptr)?)
    } else {
        None
    };
    let t = if t_ptr != 0 {
        Some(read_f32s::<M, 3>(mem, t_ptr)?)
    } else {
        None
    };
    let out = mat_transformation_val(sc, sr, s, rc, r, t);
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_transformation_2d<M: Mem>(
    mem: &mut M,
    [out_ptr, sc_ptr, sr_angle, s_ptr, rc_ptr, r_angle, t_ptr]: [u32; 7],
) -> Result<Ret, Stop> {
    let sc = if sc_ptr != 0 {
        let v = read_f32s::<M, 2>(mem, sc_ptr)?;
        Some([v[0], v[1], 0.0])
    } else {
        None
    };
    let s = if s_ptr != 0 {
        let v = read_f32s::<M, 2>(mem, s_ptr)?;
        Some([v[0], v[1], 1.0])
    } else {
        None
    };
    let rc = if rc_ptr != 0 {
        let v = read_f32s::<M, 2>(mem, rc_ptr)?;
        Some([v[0], v[1], 0.0])
    } else {
        None
    };
    let t = if t_ptr != 0 {
        let v = read_f32s::<M, 2>(mem, t_ptr)?;
        Some([v[0], v[1], 0.0])
    } else {
        None
    };
    let r_angle = f32_arg(r_angle);
    let r = if r_angle != 0.0 {
        let (sn, cs) = (r_angle / 2.0).sin_cos();
        Some([0.0, 0.0, sn, cs])
    } else {
        None
    };
    let sr_angle = f32_arg(sr_angle);
    let sr = if sr_angle != 0.0 {
        let (sn, cs) = (sr_angle / 2.0).sin_cos();
        Some([0.0, 0.0, sn, cs])
    } else {
        None
    };
    let out = mat_transformation_val(sc, sr, s, rc, r, t);
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn matrix_translation<M: Mem>(mem: &mut M, [pout, x, y, z, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let out = mat_translation_val(f32_arg(x), f32_arg(y), f32_arg(z));
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn matrix_transpose<M: Mem>(mem: &mut M, [out_ptr, in_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let m = read_f32s::<M, 16>(mem, in_ptr)?;
    let out = [
        m[0], m[4], m[8], m[12], //
        m[1], m[5], m[9], m[13], //
        m[2], m[6], m[10], m[14], //
        m[3], m[7], m[11], m[15],
    ];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn plane_from_point_normal<M: Mem>(
    mem: &mut M,
    [pout, pvpoint, pvnormal, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let pt = read_f32s::<M, 3>(mem, pvpoint)?;
    let n = read_f32s::<M, 3>(mem, pvnormal)?;
    let out = [n[0], n[1], n[2], -vec3_dot(pt, n)];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn plane_from_points<M: Mem>(
    mem: &mut M,
    [pout, pv1, pv2, pv3, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v1 = read_f32s::<M, 3>(mem, pv1)?;
    let v2 = read_f32s::<M, 3>(mem, pv2)?;
    let v3 = read_f32s::<M, 3>(mem, pv3)?;
    let edge1 = [v2[0] - v1[0], v2[1] - v1[1], v2[2] - v1[2]];
    let edge2 = [v3[0] - v1[0], v3[1] - v1[1], v3[2] - v1[2]];
    let n = vec3_normalize_val(vec3_cross(edge1, edge2));
    let out = [n[0], n[1], n[2], -vec3_dot(v1, n)];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn plane_intersect_line<M: Mem>(
    mem: &mut M,
    [pout, pp, pv1, pv2, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let p = read_f32s::<M, 4>(mem, pp)?;
    let v1 = read_f32s::<M, 3>(mem, pv1)?;
    let v2 = read_f32s::<M, 3>(mem, pv2)?;
    let normal = [p[0], p[1], p[2]];
    let dir = [v2[0] - v1[0], v2[1] - v1[1], v2[2] - v1[2]];
    let dot = vec3_dot(normal, dir);
    if dot == 0.0 {
        return Ok(0u32.into());
    }
    let temp = (p[3] + vec3_dot(normal, v1)) / dot;
    let out = [
        v1[0] - temp * dir[0],
        v1[1] - temp * dir[1],
        v1[2] - temp * dir[2],
    ];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn plane_normalize<M: Mem>(mem: &mut M, [out_ptr, p_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let p = read_f32s::<M, 4>(mem, p_ptr)?;
    write_f32s(mem, out_ptr, &plane_normalize_val(p))?;
    Ok(out_ptr.into())
}

fn plane_transform<M: Mem>(mem: &mut M, [pout, pplane, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let p = read_f32s::<M, 4>(mem, pplane)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &plane_transform_val(p, &m))?;
    Ok(pout.into())
}

fn plane_transform_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 4, 4>(mem, args, plane_transform_val)
}

fn quaternion_bary_centric<M: Mem>(
    mem: &mut M,
    [pout, pq1, pq2, pq3, f, g, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let q1 = read_f32s::<M, 4>(mem, pq1)?;
    let q2 = read_f32s::<M, 4>(mem, pq2)?;
    let q3 = read_f32s::<M, 4>(mem, pq3)?;
    let (f, g) = (f32_arg(f), f32_arg(g));
    let fg = f + g;
    let t1 = quat_slerp_val(q1, q2, fg);
    let t2 = quat_slerp_val(q1, q3, fg);
    let out = quat_slerp_val(t1, t2, g / fg);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn quaternion_exp<M: Mem>(mem: &mut M, [out_ptr, q_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q = read_f32s::<M, 4>(mem, q_ptr)?;
    write_f32s(mem, out_ptr, &quat_exp_val(q))?;
    Ok(out_ptr.into())
}

fn quaternion_inverse<M: Mem>(mem: &mut M, [pout, pq, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q = read_f32s::<M, 4>(mem, pq)?;
    write_f32s(mem, pout, &quat_inverse_val(q))?;
    Ok(pout.into())
}

fn quaternion_ln<M: Mem>(mem: &mut M, [out_ptr, q_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q = read_f32s::<M, 4>(mem, q_ptr)?;
    write_f32s(mem, out_ptr, &quat_ln_val(q))?;
    Ok(out_ptr.into())
}

fn quaternion_multiply<M: Mem>(mem: &mut M, [pout, pq1, pq2, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q1 = read_f32s::<M, 4>(mem, pq1)?;
    let q2 = read_f32s::<M, 4>(mem, pq2)?;
    write_f32s(mem, pout, &quat_mul_val(q1, q2))?;
    Ok(pout.into())
}

fn quaternion_normalize<M: Mem>(mem: &mut M, [out_ptr, q_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let q = read_f32s::<M, 4>(mem, q_ptr)?;
    let norm = quat_dot(q, q).sqrt();
    let out = [q[0] / norm, q[1] / norm, q[2] / norm, q[3] / norm];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn quaternion_rotation_axis<M: Mem>(
    mem: &mut M,
    [out_ptr, v_ptr, angle, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v = vec3_normalize_val(read_f32s::<M, 3>(mem, v_ptr)?);
    let (s, c) = (f32_arg(angle) / 2.0).sin_cos();
    let out = [s * v[0], s * v[1], s * v[2], c];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn quaternion_rotation_matrix<M: Mem>(
    mem: &mut M,
    [out_ptr, m_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let m = read_f32s::<M, 16>(mem, m_ptr)?;
    write_f32s(mem, out_ptr, &quat_rotation_matrix_val(&m))?;
    Ok(out_ptr.into())
}

fn quaternion_rotation_yaw_pitch_roll<M: Mem>(
    mem: &mut M,
    [out_ptr, yaw, pitch, roll, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let (syaw, cyaw) = (f32_arg(yaw) / 2.0).sin_cos();
    let (spitch, cpitch) = (f32_arg(pitch) / 2.0).sin_cos();
    let (sroll, croll) = (f32_arg(roll) / 2.0).sin_cos();
    let out = [
        syaw * cpitch * sroll + cyaw * spitch * croll,
        syaw * cpitch * croll - cyaw * spitch * sroll,
        cyaw * cpitch * sroll - syaw * spitch * croll,
        cyaw * cpitch * croll + syaw * spitch * sroll,
    ];
    write_f32s(mem, out_ptr, &out)?;
    Ok(out_ptr.into())
}

fn quaternion_slerp<M: Mem>(
    mem: &mut M,
    [out_ptr, q1_ptr, q2_ptr, t, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let q1 = read_f32s::<M, 4>(mem, q1_ptr)?;
    let q2 = read_f32s::<M, 4>(mem, q2_ptr)?;
    write_f32s(mem, out_ptr, &quat_slerp_val(q1, q2, f32_arg(t)))?;
    Ok(out_ptr.into())
}

fn quaternion_squad<M: Mem>(
    mem: &mut M,
    [pout, pq1, pq2, pq3, pq4, t, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let q1 = read_f32s::<M, 4>(mem, pq1)?;
    let q2 = read_f32s::<M, 4>(mem, pq2)?;
    let q3 = read_f32s::<M, 4>(mem, pq3)?;
    let q4 = read_f32s::<M, 4>(mem, pq4)?;
    let t = f32_arg(t);
    let t1 = quat_slerp_val(q1, q4, t);
    let t2 = quat_slerp_val(q2, q3, t);
    let out = quat_slerp_val(t1, t2, 2.0 * t * (1.0 - t));
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn quaternion_squad_setup<M: Mem>(
    mem: &mut M,
    [paout, pbout, pcout, pq0, pq1, pq2, pq3]: [u32; 7],
) -> Result<Ret, Stop> {
    let q0 = read_f32s::<M, 4>(mem, pq0)?;
    let q1 = read_f32s::<M, 4>(mem, pq1)?;
    let q2 = read_f32s::<M, 4>(mem, pq2)?;
    let q3 = read_f32s::<M, 4>(mem, pq3)?;
    let add_diff = |a: Quat, b: Quat, s: f32| -> Quat {
        [
            a[0] + s * b[0],
            a[1] + s * b[1],
            a[2] + s * b[2],
            a[3] + s * b[3],
        ]
    };
    let zero = [0.0f32; 4];
    let temp2 = if quat_dot(q0, q1) < 0.0 {
        add_diff(zero, q0, -1.0)
    } else {
        q0
    };
    let cout = if quat_dot(q1, q2) < 0.0 {
        add_diff(zero, q2, -1.0)
    } else {
        q2
    };
    let temp3 = if quat_dot(cout, q3) < 0.0 {
        add_diff(zero, q3, -1.0)
    } else {
        q3
    };

    let inv1 = quat_inverse_val(q1);
    let l1 = quat_ln_val(quat_mul_val(inv1, temp2));
    let l2 = quat_ln_val(quat_mul_val(inv1, cout));
    let s1 = add_diff(l1, l2, 1.0);
    let e1 = quat_exp_val([s1[0] * -0.25, s1[1] * -0.25, s1[2] * -0.25, s1[3] * -0.25]);
    let aout = quat_mul_val(q1, e1);

    let inv2 = quat_inverse_val(cout);
    let l3 = quat_ln_val(quat_mul_val(inv2, q1));
    let l4 = quat_ln_val(quat_mul_val(inv2, temp3));
    let s2 = add_diff(l3, l4, 1.0);
    let e2 = quat_exp_val([s2[0] * -0.25, s2[1] * -0.25, s2[2] * -0.25, s2[3] * -0.25]);
    let bout = quat_mul_val(cout, e2);

    let [pb, pa, pc] = mem.spans([(pbout, 16, true), (paout, 16, true), (pcout, 16, true)])?;
    // SAFETY: all three spans verified and share live aliasing.
    unsafe {
        store_f32s(pb, &bout);
        store_f32s(pa, &aout);
        store_f32s(pc, &cout);
    }
    mem.commit()?;
    Ok(Ret::default())
}

fn quaternion_to_axis_angle<M: Mem>(
    mem: &mut M,
    [pq, paxis, pangle, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    if paxis == 0 && pangle == 0 {
        return Ok(Ret::default());
    }
    let q = read_f32s::<M, 4>(mem, pq)?;
    let axis = [q[0], q[1], q[2]];
    let angle = [2.0 * q[3].acos()];
    match (paxis != 0, pangle != 0) {
        (true, true) => {
            let [pa, pan] = mem.spans([(paxis, 12, true), (pangle, 4, true)])?;
            // SAFETY: both spans verified and share live aliasing.
            unsafe {
                store_f32s(pa, &axis);
                store_f32s(pan, &angle);
            }
            mem.commit()?;
        }
        (true, false) => write_f32s(mem, paxis, &axis)?,
        (false, true) => write_f32s(mem, pangle, &angle)?,
        (false, false) => {}
    }
    Ok(Ret::default())
}

fn vec_bary_centric<M: Mem, const N: usize>(
    mem: &mut M,
    [pout, pv1, pv2, pv3, f, g, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v1 = read_f32s::<M, N>(mem, pv1)?;
    let v2 = read_f32s::<M, N>(mem, pv2)?;
    let v3 = read_f32s::<M, N>(mem, pv3)?;
    let (f, g) = (f32_arg(f), f32_arg(g));
    let w1 = 1.0 - f - g;
    let out = std::array::from_fn::<f32, N, _>(|i| w1 * v1[i] + f * v2[i] + g * v3[i]);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec_catmull_rom<M: Mem, const N: usize>(
    mem: &mut M,
    [pout, pv0, pv1, pv2, pv3, s, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v0 = read_f32s::<M, N>(mem, pv0)?;
    let v1 = read_f32s::<M, N>(mem, pv1)?;
    let v2 = read_f32s::<M, N>(mem, pv2)?;
    let v3 = read_f32s::<M, N>(mem, pv3)?;
    let s = f32_arg(s);
    let out = std::array::from_fn::<f32, N, _>(|i| {
        0.5 * (2.0 * v1[i]
            + (v2[i] - v0[i]) * s
            + (2.0 * v0[i] - 5.0 * v1[i] + 4.0 * v2[i] - v3[i]) * s * s
            + (v3[i] - 3.0 * v2[i] + 3.0 * v1[i] - v0[i]) * s * s * s)
    });
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec_hermite<M: Mem, const N: usize>(
    mem: &mut M,
    [pout, pv1, pt1, pv2, pt2, s, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v1 = read_f32s::<M, N>(mem, pv1)?;
    let t1 = read_f32s::<M, N>(mem, pt1)?;
    let v2 = read_f32s::<M, N>(mem, pv2)?;
    let t2 = read_f32s::<M, N>(mem, pt2)?;
    let s = f32_arg(s);
    let h1 = 2.0 * s * s * s - 3.0 * s * s + 1.0;
    let h2 = s * s * s - 2.0 * s * s + s;
    let h3 = -2.0 * s * s * s + 3.0 * s * s;
    let h4 = s * s * s - s * s;
    let out =
        std::array::from_fn::<f32, N, _>(|i| h1 * v1[i] + h2 * t1[i] + h3 * v2[i] + h4 * t2[i]);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec2_normalize<M: Mem>(mem: &mut M, [pout, pv, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 2>(mem, pv)?;
    let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
    let out = if norm == 0.0 {
        [0.0, 0.0]
    } else {
        [v[0] / norm, v[1] / norm]
    };
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec2_transform<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 2>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec2_transform_val(v, &m))?;
    Ok(pout.into())
}

fn vec2_transform_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 2, 4>(mem, args, vec2_transform_val)
}

fn vec2_transform_coord<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 2>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec2_transform_coord_val(v, &m))?;
    Ok(pout.into())
}

fn vec2_transform_coord_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 2, 2>(mem, args, vec2_transform_coord_val)
}

fn vec2_transform_normal<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 2>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec2_transform_normal_val(v, &m))?;
    Ok(pout.into())
}

fn vec2_transform_normal_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 2, 2>(mem, args, vec2_transform_normal_val)
}

fn vec3_normalize<M: Mem>(mem: &mut M, [pout, pv, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    write_f32s(mem, pout, &vec3_normalize_val(v))?;
    Ok(pout.into())
}

fn vec3_project<M: Mem>(
    mem: &mut M,
    [pout, pv, pvp, pproj, pview, pworld, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    let mut m = IDENTITY;
    if pworld != 0 {
        let w = read_f32s::<M, 16>(mem, pworld)?;
        m = mat_mul_val(&m, &w);
    }
    if pview != 0 {
        let vw = read_f32s::<M, 16>(mem, pview)?;
        m = mat_mul_val(&m, &vw);
    }
    if pproj != 0 {
        let pr = read_f32s::<M, 16>(mem, pproj)?;
        m = mat_mul_val(&m, &pr);
    }
    let vp = if pvp != 0 {
        Some(read_viewport(mem, pvp)?)
    } else {
        None
    };
    let mut out = vec3_transform_coord_val(v, &m);
    if let Some(vp) = vp {
        out[0] = vp.x + (1.0 + out[0]) * vp.width / 2.0;
        out[1] = vp.y + (1.0 - out[1]) * vp.height / 2.0;
        out[2] = vp.min_z + out[2] * (vp.max_z - vp.min_z);
    }
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec3_unproject<M: Mem>(
    mem: &mut M,
    [pout, pv, pvp, pproj, pview, pworld, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    let mut m = IDENTITY;
    if pworld != 0 {
        let w = read_f32s::<M, 16>(mem, pworld)?;
        m = mat_mul_val(&m, &w);
    }
    if pview != 0 {
        let vw = read_f32s::<M, 16>(mem, pview)?;
        m = mat_mul_val(&m, &vw);
    }
    if pproj != 0 {
        let pr = read_f32s::<M, 16>(mem, pproj)?;
        m = mat_mul_val(&m, &pr);
    }
    let vp = if pvp != 0 {
        Some(read_viewport(mem, pvp)?)
    } else {
        None
    };
    if let Some((inv, _)) = mat_inverse_val(&m) {
        m = inv;
    }
    let mut cur = v;
    if let Some(vp) = vp {
        cur[0] = 2.0 * (cur[0] - vp.x) / vp.width - 1.0;
        cur[1] = 1.0 - 2.0 * (cur[1] - vp.y) / vp.height;
        cur[2] = (cur[2] - vp.min_z) / (vp.max_z - vp.min_z);
    }
    let out = vec3_transform_coord_val(cur, &m);
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec3_transform<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec3_transform_val(v, &m))?;
    Ok(pout.into())
}

fn vec3_transform_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 3, 4>(mem, args, vec3_transform_val)
}

fn vec3_transform_coord<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec3_transform_coord_val(v, &m))?;
    Ok(pout.into())
}

fn vec3_transform_coord_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 3, 3>(mem, args, vec3_transform_coord_val)
}

fn vec3_transform_normal<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 3>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec3_transform_normal_val(v, &m))?;
    Ok(pout.into())
}

fn vec3_transform_normal_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 3, 3>(mem, args, vec3_transform_normal_val)
}

fn vec4_cross<M: Mem>(mem: &mut M, [pout, pv1, pv2, pv3, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v1 = read_f32s::<M, 4>(mem, pv1)?;
    let v2 = read_f32s::<M, 4>(mem, pv2)?;
    let v3 = read_f32s::<M, 4>(mem, pv3)?;
    let out = [
        v1[1] * (v2[2] * v3[3] - v3[2] * v2[3]) - v1[2] * (v2[1] * v3[3] - v3[1] * v2[3])
            + v1[3] * (v2[1] * v3[2] - v2[2] * v3[1]),
        -(v1[0] * (v2[2] * v3[3] - v3[2] * v2[3]) - v1[2] * (v2[0] * v3[3] - v3[0] * v2[3])
            + v1[3] * (v2[0] * v3[2] - v3[0] * v2[2])),
        v1[0] * (v2[1] * v3[3] - v3[1] * v2[3]) - v1[1] * (v2[0] * v3[3] - v3[0] * v2[3])
            + v1[3] * (v2[0] * v3[1] - v3[0] * v2[1]),
        -(v1[0] * (v2[1] * v3[2] - v3[1] * v2[2]) - v1[1] * (v2[0] * v3[2] - v3[0] * v2[2])
            + v1[2] * (v2[0] * v3[1] - v3[0] * v2[1])),
    ];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec4_normalize<M: Mem>(mem: &mut M, [pout, pv, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 4>(mem, pv)?;
    let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
    let out = [v[0] / norm, v[1] / norm, v[2] / norm, v[3] / norm];
    write_f32s(mem, pout, &out)?;
    Ok(pout.into())
}

fn vec4_transform<M: Mem>(mem: &mut M, [pout, pv, pm, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = read_f32s::<M, 4>(mem, pv)?;
    let m = read_f32s::<M, 16>(mem, pm)?;
    write_f32s(mem, pout, &vec4_transform_val(v, &m))?;
    Ok(pout.into())
}

fn vec4_transform_array<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    transform_array::<M, 4, 4>(mem, args, vec4_transform_val)
}

// ============================================================================
// DirectBody descriptors
// ============================================================================

macro_rules! d3dx_body {
    ($name:ident, $func:path, 2, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN2, outputs: $out, pop: 8);
    };
    ($name:ident, $func:path, 3, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN3, outputs: $out, pop: 12);
    };
    ($name:ident, $func:path, 4, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN4, outputs: $out, pop: 16);
    };
    ($name:ident, $func:path, 5, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN5, outputs: $out, pop: 20);
    };
    ($name:ident, $func:path, 6, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN6, outputs: $out, pop: 24);
    };
    ($name:ident, $func:path, 7, $out:expr) => {
        adv_cpu::body!($name = $func, inputs: IN7, outputs: $out, pop: 28);
    };
}

d3dx_body!(COLOR_ADJUST_CONTRAST, color_adjust_contrast, 3, EAX);
d3dx_body!(COLOR_ADJUST_SATURATION, color_adjust_saturation, 3, EAX);
d3dx_body!(
    MATRIX_AFFINE_TRANSFORMATION,
    matrix_affine_transformation,
    5,
    EAX
);
d3dx_body!(
    MATRIX_AFFINE_TRANSFORMATION_2D,
    matrix_affine_transformation_2d,
    5,
    EAX
);
d3dx_body!(MATRIX_DECOMPOSE, matrix_decompose, 4, EAX);
d3dx_body!(MATRIX_INVERSE, matrix_inverse, 3, EAX);
d3dx_body!(MATRIX_LOOK_AT_LH, matrix_look_at::<_, false>, 4, EAX);
d3dx_body!(MATRIX_LOOK_AT_RH, matrix_look_at::<_, true>, 4, EAX);
d3dx_body!(MATRIX_MULTIPLY, matrix_multiply, 3, EAX);
d3dx_body!(MATRIX_MULTIPLY_TRANSPOSE, matrix_multiply_transpose, 3, EAX);
d3dx_body!(MATRIX_ORTHO_LH, matrix_ortho::<_, false>, 5, EAX);
d3dx_body!(MATRIX_ORTHO_RH, matrix_ortho::<_, true>, 5, EAX);
d3dx_body!(
    MATRIX_ORTHO_OFF_CENTER_LH,
    matrix_ortho_off_center::<_, false>,
    7,
    EAX
);
d3dx_body!(
    MATRIX_ORTHO_OFF_CENTER_RH,
    matrix_ortho_off_center::<_, true>,
    7,
    EAX
);
d3dx_body!(
    MATRIX_PERSPECTIVE_FOV_LH,
    matrix_perspective_fov::<_, false>,
    5,
    EAX
);
d3dx_body!(
    MATRIX_PERSPECTIVE_FOV_RH,
    matrix_perspective_fov::<_, true>,
    5,
    EAX
);
d3dx_body!(
    MATRIX_PERSPECTIVE_LH,
    matrix_perspective::<_, false>,
    5,
    EAX
);
d3dx_body!(MATRIX_PERSPECTIVE_RH, matrix_perspective::<_, true>, 5, EAX);
d3dx_body!(
    MATRIX_PERSPECTIVE_OFF_CENTER_LH,
    matrix_perspective_off_center::<_, false>,
    7,
    EAX
);
d3dx_body!(
    MATRIX_PERSPECTIVE_OFF_CENTER_RH,
    matrix_perspective_off_center::<_, true>,
    7,
    EAX
);
d3dx_body!(MATRIX_REFLECT, matrix_reflect, 2, EAX);
d3dx_body!(MATRIX_ROTATION_AXIS, matrix_rotation_axis, 3, EAX);
d3dx_body!(
    MATRIX_ROTATION_QUATERNION,
    matrix_rotation_quaternion,
    2,
    EAX
);
d3dx_body!(MATRIX_ROTATION_X, matrix_rotation_x, 2, EAX);
d3dx_body!(MATRIX_ROTATION_Y, matrix_rotation_y, 2, EAX);
d3dx_body!(MATRIX_ROTATION_Z, matrix_rotation_z, 2, EAX);
d3dx_body!(MATRIX_ROTATION_YPR, matrix_rotation_yaw_pitch_roll, 4, EAX);
d3dx_body!(MATRIX_SCALING, matrix_scaling, 4, EAX);
d3dx_body!(MATRIX_SHADOW, matrix_shadow, 3, EAX);
d3dx_body!(MATRIX_TRANSFORMATION, matrix_transformation, 7, EAX);
d3dx_body!(MATRIX_TRANSFORMATION_2D, matrix_transformation_2d, 7, EAX);
d3dx_body!(MATRIX_TRANSLATION, matrix_translation, 4, EAX);
d3dx_body!(MATRIX_TRANSPOSE, matrix_transpose, 2, EAX);
d3dx_body!(PLANE_FROM_POINT_NORMAL, plane_from_point_normal, 3, EAX);
d3dx_body!(PLANE_FROM_POINTS, plane_from_points, 4, EAX);
d3dx_body!(PLANE_INTERSECT_LINE, plane_intersect_line, 4, EAX);
d3dx_body!(PLANE_NORMALIZE, plane_normalize, 2, EAX);
d3dx_body!(PLANE_TRANSFORM, plane_transform, 3, EAX);
d3dx_body!(PLANE_TRANSFORM_ARRAY, plane_transform_array, 6, EAX);
d3dx_body!(QUATERNION_BARY_CENTRIC, quaternion_bary_centric, 6, EAX);
d3dx_body!(QUATERNION_EXP, quaternion_exp, 2, EAX);
d3dx_body!(QUATERNION_INVERSE, quaternion_inverse, 2, EAX);
d3dx_body!(QUATERNION_LN, quaternion_ln, 2, EAX);
d3dx_body!(QUATERNION_MULTIPLY, quaternion_multiply, 3, EAX);
d3dx_body!(QUATERNION_NORMALIZE, quaternion_normalize, 2, EAX);
d3dx_body!(QUATERNION_ROTATION_AXIS, quaternion_rotation_axis, 3, EAX);
d3dx_body!(
    QUATERNION_ROTATION_MATRIX,
    quaternion_rotation_matrix,
    2,
    EAX
);
d3dx_body!(
    QUATERNION_ROTATION_YPR,
    quaternion_rotation_yaw_pitch_roll,
    4,
    EAX
);
d3dx_body!(QUATERNION_SLERP, quaternion_slerp, 4, EAX);
d3dx_body!(QUATERNION_SQUAD, quaternion_squad, 6, EAX);
d3dx_body!(QUATERNION_SQUAD_SETUP, quaternion_squad_setup, 7, 0);
d3dx_body!(QUATERNION_TO_AXIS_ANGLE, quaternion_to_axis_angle, 3, 0);
d3dx_body!(VEC2_BARY_CENTRIC, vec_bary_centric::<_, 2>, 6, EAX);
d3dx_body!(VEC2_CATMULL_ROM, vec_catmull_rom::<_, 2>, 6, EAX);
d3dx_body!(VEC2_HERMITE, vec_hermite::<_, 2>, 6, EAX);
d3dx_body!(VEC2_NORMALIZE, vec2_normalize, 2, EAX);
d3dx_body!(VEC2_TRANSFORM, vec2_transform, 3, EAX);
d3dx_body!(VEC2_TRANSFORM_ARRAY, vec2_transform_array, 6, EAX);
d3dx_body!(VEC2_TRANSFORM_COORD, vec2_transform_coord, 3, EAX);
d3dx_body!(
    VEC2_TRANSFORM_COORD_ARRAY,
    vec2_transform_coord_array,
    6,
    EAX
);
d3dx_body!(VEC2_TRANSFORM_NORMAL, vec2_transform_normal, 3, EAX);
d3dx_body!(
    VEC2_TRANSFORM_NORMAL_ARRAY,
    vec2_transform_normal_array,
    6,
    EAX
);
d3dx_body!(VEC3_BARY_CENTRIC, vec_bary_centric::<_, 3>, 6, EAX);
d3dx_body!(VEC3_CATMULL_ROM, vec_catmull_rom::<_, 3>, 6, EAX);
d3dx_body!(VEC3_HERMITE, vec_hermite::<_, 3>, 6, EAX);
d3dx_body!(VEC3_NORMALIZE, vec3_normalize, 2, EAX);
d3dx_body!(VEC3_PROJECT, vec3_project, 6, EAX);
d3dx_body!(VEC3_UNPROJECT, vec3_unproject, 6, EAX);
d3dx_body!(VEC3_TRANSFORM, vec3_transform, 3, EAX);
d3dx_body!(VEC3_TRANSFORM_ARRAY, vec3_transform_array, 6, EAX);
d3dx_body!(VEC3_TRANSFORM_COORD, vec3_transform_coord, 3, EAX);
d3dx_body!(
    VEC3_TRANSFORM_COORD_ARRAY,
    vec3_transform_coord_array,
    6,
    EAX
);
d3dx_body!(VEC3_TRANSFORM_NORMAL, vec3_transform_normal, 3, EAX);
d3dx_body!(
    VEC3_TRANSFORM_NORMAL_ARRAY,
    vec3_transform_normal_array,
    6,
    EAX
);
d3dx_body!(VEC4_BARY_CENTRIC, vec_bary_centric::<_, 4>, 6, EAX);
d3dx_body!(VEC4_CATMULL_ROM, vec_catmull_rom::<_, 4>, 6, EAX);
d3dx_body!(VEC4_CROSS, vec4_cross, 4, EAX);
d3dx_body!(VEC4_HERMITE, vec_hermite::<_, 4>, 6, EAX);
d3dx_body!(VEC4_NORMALIZE, vec4_normalize, 2, EAX);
d3dx_body!(VEC4_TRANSFORM, vec4_transform, 3, EAX);
d3dx_body!(VEC4_TRANSFORM_ARRAY, vec4_transform_array, 6, EAX);

pub(super) const TABLE: &[(&str, u8, adv_cpu::body::Body)] = &[
    ("D3DXColorAdjustContrast", 3, COLOR_ADJUST_CONTRAST),
    ("D3DXColorAdjustSaturation", 3, COLOR_ADJUST_SATURATION),
    (
        "D3DXMatrixAffineTransformation",
        5,
        MATRIX_AFFINE_TRANSFORMATION,
    ),
    (
        "D3DXMatrixAffineTransformation2D",
        5,
        MATRIX_AFFINE_TRANSFORMATION_2D,
    ),
    ("D3DXMatrixDecompose", 4, MATRIX_DECOMPOSE),
    ("D3DXMatrixInverse", 3, MATRIX_INVERSE),
    ("D3DXMatrixLookAtLH", 4, MATRIX_LOOK_AT_LH),
    ("D3DXMatrixLookAtRH", 4, MATRIX_LOOK_AT_RH),
    ("D3DXMatrixMultiply", 3, MATRIX_MULTIPLY),
    ("D3DXMatrixMultiplyTranspose", 3, MATRIX_MULTIPLY_TRANSPOSE),
    ("D3DXMatrixOrthoLH", 5, MATRIX_ORTHO_LH),
    ("D3DXMatrixOrthoOffCenterLH", 7, MATRIX_ORTHO_OFF_CENTER_LH),
    ("D3DXMatrixOrthoOffCenterRH", 7, MATRIX_ORTHO_OFF_CENTER_RH),
    ("D3DXMatrixOrthoRH", 5, MATRIX_ORTHO_RH),
    ("D3DXMatrixPerspectiveFovLH", 5, MATRIX_PERSPECTIVE_FOV_LH),
    ("D3DXMatrixPerspectiveFovRH", 5, MATRIX_PERSPECTIVE_FOV_RH),
    ("D3DXMatrixPerspectiveLH", 5, MATRIX_PERSPECTIVE_LH),
    (
        "D3DXMatrixPerspectiveOffCenterLH",
        7,
        MATRIX_PERSPECTIVE_OFF_CENTER_LH,
    ),
    (
        "D3DXMatrixPerspectiveOffCenterRH",
        7,
        MATRIX_PERSPECTIVE_OFF_CENTER_RH,
    ),
    ("D3DXMatrixPerspectiveRH", 5, MATRIX_PERSPECTIVE_RH),
    ("D3DXMatrixReflect", 2, MATRIX_REFLECT),
    ("D3DXMatrixRotationAxis", 3, MATRIX_ROTATION_AXIS),
    (
        "D3DXMatrixRotationQuaternion",
        2,
        MATRIX_ROTATION_QUATERNION,
    ),
    ("D3DXMatrixRotationX", 2, MATRIX_ROTATION_X),
    ("D3DXMatrixRotationY", 2, MATRIX_ROTATION_Y),
    ("D3DXMatrixRotationYawPitchRoll", 4, MATRIX_ROTATION_YPR),
    ("D3DXMatrixRotationZ", 2, MATRIX_ROTATION_Z),
    ("D3DXMatrixScaling", 4, MATRIX_SCALING),
    ("D3DXMatrixShadow", 3, MATRIX_SHADOW),
    ("D3DXMatrixTransformation", 7, MATRIX_TRANSFORMATION),
    ("D3DXMatrixTransformation2D", 7, MATRIX_TRANSFORMATION_2D),
    ("D3DXMatrixTranslation", 4, MATRIX_TRANSLATION),
    ("D3DXMatrixTranspose", 2, MATRIX_TRANSPOSE),
    ("D3DXPlaneFromPointNormal", 3, PLANE_FROM_POINT_NORMAL),
    ("D3DXPlaneFromPoints", 4, PLANE_FROM_POINTS),
    ("D3DXPlaneIntersectLine", 4, PLANE_INTERSECT_LINE),
    ("D3DXPlaneNormalize", 2, PLANE_NORMALIZE),
    ("D3DXPlaneTransform", 3, PLANE_TRANSFORM),
    ("D3DXPlaneTransformArray", 6, PLANE_TRANSFORM_ARRAY),
    ("D3DXQuaternionBaryCentric", 6, QUATERNION_BARY_CENTRIC),
    ("D3DXQuaternionExp", 2, QUATERNION_EXP),
    ("D3DXQuaternionInverse", 2, QUATERNION_INVERSE),
    ("D3DXQuaternionLn", 2, QUATERNION_LN),
    ("D3DXQuaternionMultiply", 3, QUATERNION_MULTIPLY),
    ("D3DXQuaternionNormalize", 2, QUATERNION_NORMALIZE),
    ("D3DXQuaternionRotationAxis", 3, QUATERNION_ROTATION_AXIS),
    (
        "D3DXQuaternionRotationMatrix",
        2,
        QUATERNION_ROTATION_MATRIX,
    ),
    (
        "D3DXQuaternionRotationYawPitchRoll",
        4,
        QUATERNION_ROTATION_YPR,
    ),
    ("D3DXQuaternionSlerp", 4, QUATERNION_SLERP),
    ("D3DXQuaternionSquad", 6, QUATERNION_SQUAD),
    ("D3DXQuaternionSquadSetup", 7, QUATERNION_SQUAD_SETUP),
    ("D3DXQuaternionToAxisAngle", 3, QUATERNION_TO_AXIS_ANGLE),
    ("D3DXVec2BaryCentric", 6, VEC2_BARY_CENTRIC),
    ("D3DXVec2CatmullRom", 6, VEC2_CATMULL_ROM),
    ("D3DXVec2Hermite", 6, VEC2_HERMITE),
    ("D3DXVec2Normalize", 2, VEC2_NORMALIZE),
    ("D3DXVec2Transform", 3, VEC2_TRANSFORM),
    ("D3DXVec2TransformArray", 6, VEC2_TRANSFORM_ARRAY),
    ("D3DXVec2TransformCoord", 3, VEC2_TRANSFORM_COORD),
    ("D3DXVec2TransformCoordArray", 6, VEC2_TRANSFORM_COORD_ARRAY),
    ("D3DXVec2TransformNormal", 3, VEC2_TRANSFORM_NORMAL),
    (
        "D3DXVec2TransformNormalArray",
        6,
        VEC2_TRANSFORM_NORMAL_ARRAY,
    ),
    ("D3DXVec3BaryCentric", 6, VEC3_BARY_CENTRIC),
    ("D3DXVec3CatmullRom", 6, VEC3_CATMULL_ROM),
    ("D3DXVec3Hermite", 6, VEC3_HERMITE),
    ("D3DXVec3Normalize", 2, VEC3_NORMALIZE),
    ("D3DXVec3Project", 6, VEC3_PROJECT),
    ("D3DXVec3Transform", 3, VEC3_TRANSFORM),
    ("D3DXVec3TransformArray", 6, VEC3_TRANSFORM_ARRAY),
    ("D3DXVec3TransformCoord", 3, VEC3_TRANSFORM_COORD),
    ("D3DXVec3TransformCoordArray", 6, VEC3_TRANSFORM_COORD_ARRAY),
    ("D3DXVec3TransformNormal", 3, VEC3_TRANSFORM_NORMAL),
    (
        "D3DXVec3TransformNormalArray",
        6,
        VEC3_TRANSFORM_NORMAL_ARRAY,
    ),
    ("D3DXVec3Unproject", 6, VEC3_UNPROJECT),
    ("D3DXVec4BaryCentric", 6, VEC4_BARY_CENTRIC),
    ("D3DXVec4CatmullRom", 6, VEC4_CATMULL_ROM),
    ("D3DXVec4Cross", 4, VEC4_CROSS),
    ("D3DXVec4Hermite", 6, VEC4_HERMITE),
    ("D3DXVec4Normalize", 2, VEC4_NORMALIZE),
    ("D3DXVec4Transform", 3, VEC4_TRANSFORM),
    ("D3DXVec4TransformArray", 6, VEC4_TRANSFORM_ARRAY),
];

pub(super) fn bodies() -> Vec<Body> {
    let mut bodies = Vec::with_capacity(20 * TABLE.len());
    for version in 24..=43 {
        for &(name, argc, direct) in TABLE {
            bodies.push(Body {
                name: format!("d3dx9_{version}.dll!{name}").into(),
                conv: Conv::Stdcall { argc },
                body: None,
                handler: resume_original,
                direct: Some(direct),
            });
        }
    }
    bodies
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::{Guest, Prot};
    use adv_cpu::body::{GuestMem, Scratch};

    fn run_body(
        body: adv_cpu::body::Body,
        cpu: &mut adv_cpu::Cpu,
        args: [u32; 7],
    ) -> Result<Ret, Stop> {
        let mut scratch = Scratch::default();
        (body.full)(&mut GuestMem::new(cpu, &mut scratch), args)
    }

    fn put_f32s(cpu: &mut adv_cpu::Cpu, at: u32, vals: &[f32]) {
        let mut bytes = Vec::with_capacity(vals.len() * 4);
        for &v in vals {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        cpu.write(at, &bytes).unwrap();
    }

    fn get_f32s(cpu: &adv_cpu::Cpu, at: u32, n: usize) -> Vec<f32> {
        let mut bytes = vec![0u8; n * 4];
        cpu.read(at, &mut bytes).unwrap();
        (0..n)
            .map(|i| f32::from_le_bytes(bytes[i * 4..(i + 1) * 4].try_into().unwrap()))
            .collect()
    }

    // spec: A64F-FP-10, NAT-BODY-16, NAT-BODY-17, NAT-BODY-18
    #[test]
    fn d3dx_math_bodies_handle_identities_aliasing_special_values_and_faults() {
        let mut cpu = adv_cpu::Cpu::new(0x1000, false).unwrap();
        cpu.map(0x1_0000, 0x2000, Prot::RW).unwrap();

        // RotationX(angle) * RotationX(-angle) == Identity within 4 ULP.
        let m_pos = 0x1_0000;
        let m_neg = 0x1_0040;
        let m_prod = 0x1_0080;
        let angle = std::f32::consts::FRAC_PI_4;
        assert_eq!(
            run_body(
                MATRIX_ROTATION_X,
                &mut cpu,
                [m_pos, angle.to_bits(), 0, 0, 0, 0, 0]
            )
            .unwrap()
            .value as u32,
            m_pos
        );
        assert_eq!(
            run_body(
                MATRIX_ROTATION_X,
                &mut cpu,
                [m_neg, (-angle).to_bits(), 0, 0, 0, 0, 0]
            )
            .unwrap()
            .value as u32,
            m_neg
        );
        assert_eq!(
            run_body(
                MATRIX_MULTIPLY,
                &mut cpu,
                [m_prod, m_pos, m_neg, 0, 0, 0, 0]
            )
            .unwrap()
            .value as u32,
            m_prod
        );
        let prod = get_f32s(&cpu, m_prod, 16);
        for (i, (&actual, &expected)) in prod.iter().zip(IDENTITY.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 4.0 * f32::EPSILON,
                "R(a)*R(-a)[{i}]: {actual} vs {expected}"
            );
        }

        // In-place MatrixInverse(m, &det, m) * original == Identity.
        let m_orig = 0x1_0100;
        let m_inv = 0x1_0140;
        let det_at = 0x1_0180;
        let sample: [f32; 16] = [
            2.0, 0.5, -0.25, 0.0, //
            -0.5, 3.0, 0.75, 0.0, //
            0.25, -0.5, 1.5, 0.0, //
            4.0, -5.0, 6.0, 1.0,
        ];
        put_f32s(&mut cpu, m_orig, &sample);
        put_f32s(&mut cpu, m_inv, &sample);
        assert_eq!(
            run_body(MATRIX_INVERSE, &mut cpu, [m_inv, det_at, m_inv, 0, 0, 0, 0])
                .unwrap()
                .value as u32,
            m_inv
        );
        run_body(
            MATRIX_MULTIPLY,
            &mut cpu,
            [m_prod, m_orig, m_inv, 0, 0, 0, 0],
        )
        .unwrap();
        let prod = get_f32s(&cpu, m_prod, 16);
        for (i, (&actual, &expected)) in prod.iter().zip(IDENTITY.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 16.0 * f32::EPSILON,
                "M*M^-1[{i}]: {actual} vs {expected}"
            );
        }

        // Singular matrix returns NULL without mutating output or determinant.
        let m_zero = 0x1_0200;
        put_f32s(&mut cpu, m_zero, &[0.0; 16]);
        put_f32s(&mut cpu, det_at, &[42.0]);
        assert_eq!(
            run_body(
                MATRIX_INVERSE,
                &mut cpu,
                [0x9000_0000, det_at, m_zero, 0, 0, 0, 0]
            )
            .unwrap()
            .value as u32,
            0
        );
        assert_eq!(get_f32s(&cpu, det_at, 1), vec![42.0]);

        // Zero vector normalize yields exact +0.0 for Vec2/Vec3/Plane and NaN for Vec4/Quat.
        let v_zero = 0x1_0240;
        let v_out = 0x1_0260;
        put_f32s(&mut cpu, v_zero, &[0.0, 0.0, 0.0, 0.0]);
        run_body(VEC3_NORMALIZE, &mut cpu, [v_out, v_zero, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(get_f32s(&cpu, v_out, 3), vec![0.0, 0.0, 0.0]);
        run_body(VEC4_NORMALIZE, &mut cpu, [v_out, v_zero, 0, 0, 0, 0, 0]).unwrap();
        assert!(get_f32s(&cpu, v_out, 4).iter().all(|v| v.is_nan()));

        // Strided Vec3TransformCoordArray faults before mutating when tail element is unmapped.
        let arr_in = 0x1_0300;
        let arr_out = 0x1_1ff0; // 16 bytes left in mapped region; 2nd element (stride 24) faults!
        put_f32s(&mut cpu, arr_in, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        put_f32s(&mut cpu, arr_out, &[99.0, 99.0, 99.0]);
        let err = run_body(
            VEC3_TRANSFORM_COORD_ARRAY,
            &mut cpu,
            [arr_out, 24, arr_in, 12, m_orig, 2, 0],
        )
        .unwrap_err();
        assert!(matches!(err, Stop::Fault { write: true, .. }));
        assert_eq!(get_f32s(&cpu, arr_out, 3), vec![99.0, 99.0, 99.0]);
    }
}
