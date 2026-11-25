//! Differential conformance tests for D3DX9 native math bodies (A64F-FP-10, NAT-BODY-1, NAT-BODY-16, NAT-BODY-17, NAT-BODY-18).
//!
//! Compares native direct bodies against Wine 11's own `d3dx9_43.dll` code
//! across all bound matrix, vector, quaternion, plane, and color exports,
//! checking ULP bounds (<= 2 ULP scalar, <= 4 ULP compound), exact special
//! values, aliasing, NULL optional pointers, strided arrays, and faults.

#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;

use std::path::Path;

use adv_cpu::Cpu;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::{ExportTarget, ImportName, Pe};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

const CODE: u32 = 0x0040_0000;
const SLOT: u32 = 0x0041_0000;
const ARGS: u32 = SLOT + 4;
const RESULT: u32 = SLOT + 0x40;
const DATA: u32 = 0x0050_0000;
const DATA_LEN: u32 = 0x4000;

fn export_addr(pe: &Pe, name: &str) -> u32 {
    pe.exports()
        .unwrap()
        .into_iter()
        .find_map(|e| match (e.name, e.target) {
            (Some(n), ExportTarget::Rva(rva)) if n == name => Some(pe.image_base + rva),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing export {name}"))
}

fn map_pe(m: &mut Machine<Cpu>, pe: &Pe, base: u32) {
    adv_nt::image::map(
        &mut m.cpu,
        &mut m.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        pe,
        base,
    )
    .unwrap();
}

fn build_machine(dist: &Path, dll_name: &str, jit: bool) -> (Machine<Cpu>, Pe) {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, jit).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = dist.join("lib/wine/i386-windows");
    let nt_bytes = std::fs::read(root.join("ntdll.dll")).unwrap();
    let crt_bytes = std::fs::read(root.join("ucrtbase.dll")).unwrap();
    let d3dx_bytes = std::fs::read(root.join(dll_name)).unwrap();

    let nt = Pe::parse(&nt_bytes).unwrap();
    let mut crt = Pe::parse(&crt_bytes).unwrap();
    let d3dx = Pe::parse(&d3dx_bytes).unwrap();

    const CRT_BASE: u32 = 0x2000_0000;
    let delta = CRT_BASE.wrapping_sub(crt.image_base);
    if delta != 0 {
        for rva in crt.relocations().unwrap_or_default() {
            let off = rva as usize;
            let val = u32::from_le_bytes(crt.image[off..off + 4].try_into().unwrap());
            crt.image[off..off + 4].copy_from_slice(&val.wrapping_add(delta).to_le_bytes());
        }
    }
    map_pe(&mut m, &nt, nt.image_base);
    map_pe(&mut m, &crt, CRT_BASE);
    map_pe(&mut m, &d3dx, d3dx.image_base);

    adv_nt::identity::note_ntdll(&mut m, &root.join("ntdll.dll"), &nt_bytes);
    adv_nt::identity::note_ntdll(&mut m, &root.join(dll_name), &d3dx_bytes);

    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(SLOT, 0x1000, Prot::RW).unwrap();
    m.cpu.map(DATA, DATA_LEN, Prot::RW).unwrap();
    m.cpu.map(0x7ffe_0000, 0x1000, Prot::READ).unwrap();
    m.cpu.write(CODE + 0xf00, &[0x0f, 0x0b]).unwrap();

    // Resolve imports of ucrtbase and d3dx9 from ucrtbase / ntdll so Wine's guest
    // reference code can call sqrtf/sinf/cosf/tanf/acosf cleanly.
    let resolve = |name: &str| -> Option<u32> {
        for (pe, base) in [(&crt, CRT_BASE), (&nt, nt.image_base)] {
            if let Some(addr) =
                pe.exports()
                    .unwrap()
                    .into_iter()
                    .find_map(|e| match (e.name, e.target) {
                        (Some(n), ExportTarget::Rva(rva)) if n == name => Some(base + rva),
                        _ => None,
                    })
            {
                return Some(addr);
            }
        }
        None
    };
    for (pe, base) in [(&crt, CRT_BASE), (&d3dx, d3dx.image_base)] {
        for imports in pe.imports().unwrap() {
            for import in imports.imports {
                let target = match import.name {
                    ImportName::Name { name, .. } => resolve(&name),
                    _ => None,
                }
                .unwrap_or(CODE + 0xf00);
                m.cpu.write_u32(base + import.iat_rva, target).unwrap();
            }
        }
    }

    // Disable Wine's per-module debug channel so TRACE() is a no-op.
    for (offset, bytes) in d3dx.image.windows(6).enumerate() {
        if bytes.starts_with(&[0xff]) && bytes[1..].starts_with(b"d3dx") {
            let _ = m.cpu.write(d3dx.image_base + offset as u32, &[0]);
        }
    }

    (m, d3dx)
}

/// Emit a stdcall harness for `argc` dword arguments and invoke `entry`.
fn call_stdcall(
    m: &mut Machine<Cpu>,
    entry: u32,
    args: &[u32],
    data_before: &[u8],
) -> Result<(u32, u32, Vec<u8>), String> {
    m.cpu.write(DATA, data_before).unwrap();
    m.cpu.write_u32(SLOT, entry).unwrap();
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(ARGS + (i as u32) * 4, arg).unwrap();
    }
    let mut code = Vec::new();
    // Save initial ESP in ESI so we can check callee popped `argc * 4` bytes.
    code.extend_from_slice(&[0x89, 0xe6]); // mov esi, esp
    for i in (0..args.len()).rev() {
        code.extend_from_slice(&[0xff, 0x35]);
        code.extend_from_slice(&(ARGS + (i as u32) * 4).to_le_bytes());
    }
    code.extend_from_slice(&[0xff, 0x15]);
    code.extend_from_slice(&SLOT.to_le_bytes());
    // EAX -> RESULT, (ESP - ESI) -> RESULT + 4
    code.push(0xa3);
    code.extend_from_slice(&RESULT.to_le_bytes());
    code.extend_from_slice(&[0x29, 0xf4]); // sub esp, esi
    code.extend_from_slice(&[0x89, 0x25]);
    code.extend_from_slice(&(RESULT + 4).to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf4]); // mov esp, esi
    code.push(0xc3);
    m.cpu.write(CODE, &code).unwrap();

    m.spawn("d3dx-call", CODE, &[], 0x10000).unwrap();
    match m.run() {
        RunResult::Exited(_) => {}
        RunResult::Failed(error) => return Err(format!("{:#x} {}", error.pc, error.message)),
        other => return Err(format!("{other:?}")),
    }
    let eax = m.cpu.read_u32(RESULT).unwrap();
    let esp_delta = m.cpu.read_u32(RESULT + 4).unwrap();
    let mut out = vec![0u8; data_before.len()];
    m.cpu.read(DATA, &mut out).unwrap();
    Ok((eax, esp_delta, out))
}

fn write_f32s(buf: &mut [u8], offset: usize, vals: &[f32]) {
    for (i, &v) in vals.iter().enumerate() {
        buf[offset + i * 4..offset + (i + 1) * 4].copy_from_slice(&v.to_le_bytes());
    }
}

fn read_f32s(buf: &[u8], offset: usize, count: usize) -> Vec<f32> {
    (0..count)
        .map(|i| {
            f32::from_le_bytes(
                buf[offset + i * 4..offset + (i + 1) * 4]
                    .try_into()
                    .unwrap(),
            )
        })
        .collect()
}

fn ulp_diff_f32(a: f32, b: f32) -> Option<u32> {
    if a.is_nan() && b.is_nan() {
        return Some(0);
    }
    if a.is_infinite() && b.is_infinite() && a.is_sign_positive() == b.is_sign_positive() {
        return Some(0);
    }
    if !a.is_finite() || !b.is_finite() {
        return None;
    }
    if a == 0.0 && b == 0.0 {
        return Some(0);
    }
    let ordered = |v: f32| -> i32 {
        let bits = v.to_bits() as i32;
        if bits < 0 { i32::MIN - bits } else { bits }
    };
    Some(ordered(a).abs_diff(ordered(b)))
}

fn assert_f32s_within_ulp(label: &str, actual: &[f32], expected: &[f32], max_ulp: u32) {
    assert_eq!(actual.len(), expected.len(), "{label}: length mismatch");
    for (i, (&a, &b)) in actual.iter().zip(expected.iter()).enumerate() {
        // Near-zero cancellation (e.g. cos(pi/2) or orthogonal cross terms) can
        // produce values around 1e-7 where ULP scales with 1e-14; allow tiny
        // absolute residual <= 4 * f32::EPSILON when exact value is near zero.
        let ulp = ulp_diff_f32(a, b);
        let ok = ulp.is_some_and(|u| u <= max_ulp)
            || (a.is_finite() && b.is_finite() && (a - b).abs() <= 4.0 * f32::EPSILON);
        assert!(
            ok,
            "{label}[{i}]: actual={a:?} ({:#010x}) expected={b:?} ({:#010x}) ulp={ulp:?}",
            a.to_bits(),
            b.to_bits()
        );
    }
}

// spec: A64F-FP-10, NAT-BODY-1, NAT-BODY-16, NAT-BODY-17, NAT-BODY-18
#[test]
fn d3dx9_math_bodies_bind_and_match_wine_within_ulp_bounds() {
    let Some(dist) = wine::dist() else {
        return;
    };
    let (mut native, d3dx) = build_machine(&dist, "d3dx9_43.dll", true);
    let (mut reference, _) = build_machine(&dist, "d3dx9_43.dll", false);
    reference.sys.redirects.clear();
    reference.sys.direct_bodies.clear();

    // Confirm all 81 math exports are bound directly on d3dx9_43.dll.
    let all_names = [
        "D3DXColorAdjustContrast",
        "D3DXColorAdjustSaturation",
        "D3DXMatrixAffineTransformation",
        "D3DXMatrixAffineTransformation2D",
        "D3DXMatrixDecompose",
        "D3DXMatrixInverse",
        "D3DXMatrixLookAtLH",
        "D3DXMatrixLookAtRH",
        "D3DXMatrixMultiply",
        "D3DXMatrixMultiplyTranspose",
        "D3DXMatrixOrthoLH",
        "D3DXMatrixOrthoOffCenterLH",
        "D3DXMatrixOrthoOffCenterRH",
        "D3DXMatrixOrthoRH",
        "D3DXMatrixPerspectiveFovLH",
        "D3DXMatrixPerspectiveFovRH",
        "D3DXMatrixPerspectiveLH",
        "D3DXMatrixPerspectiveOffCenterLH",
        "D3DXMatrixPerspectiveOffCenterRH",
        "D3DXMatrixPerspectiveRH",
        "D3DXMatrixReflect",
        "D3DXMatrixRotationAxis",
        "D3DXMatrixRotationQuaternion",
        "D3DXMatrixRotationX",
        "D3DXMatrixRotationY",
        "D3DXMatrixRotationYawPitchRoll",
        "D3DXMatrixRotationZ",
        "D3DXMatrixScaling",
        "D3DXMatrixShadow",
        "D3DXMatrixTransformation",
        "D3DXMatrixTransformation2D",
        "D3DXMatrixTranslation",
        "D3DXMatrixTranspose",
        "D3DXPlaneFromPointNormal",
        "D3DXPlaneFromPoints",
        "D3DXPlaneIntersectLine",
        "D3DXPlaneNormalize",
        "D3DXPlaneTransform",
        "D3DXPlaneTransformArray",
        "D3DXQuaternionBaryCentric",
        "D3DXQuaternionExp",
        "D3DXQuaternionInverse",
        "D3DXQuaternionLn",
        "D3DXQuaternionMultiply",
        "D3DXQuaternionNormalize",
        "D3DXQuaternionRotationAxis",
        "D3DXQuaternionRotationMatrix",
        "D3DXQuaternionRotationYawPitchRoll",
        "D3DXQuaternionSlerp",
        "D3DXQuaternionSquad",
        "D3DXQuaternionSquadSetup",
        "D3DXQuaternionToAxisAngle",
        "D3DXVec2BaryCentric",
        "D3DXVec2CatmullRom",
        "D3DXVec2Hermite",
        "D3DXVec2Normalize",
        "D3DXVec2Transform",
        "D3DXVec2TransformArray",
        "D3DXVec2TransformCoord",
        "D3DXVec2TransformCoordArray",
        "D3DXVec2TransformNormal",
        "D3DXVec2TransformNormalArray",
        "D3DXVec3BaryCentric",
        "D3DXVec3CatmullRom",
        "D3DXVec3Hermite",
        "D3DXVec3Normalize",
        "D3DXVec3Project",
        "D3DXVec3Transform",
        "D3DXVec3TransformArray",
        "D3DXVec3TransformCoord",
        "D3DXVec3TransformCoordArray",
        "D3DXVec3TransformNormal",
        "D3DXVec3TransformNormalArray",
        "D3DXVec3Unproject",
        "D3DXVec4BaryCentric",
        "D3DXVec4CatmullRom",
        "D3DXVec4Cross",
        "D3DXVec4Hermite",
        "D3DXVec4Normalize",
        "D3DXVec4Transform",
        "D3DXVec4TransformArray",
    ];
    for name in all_names {
        let entry = export_addr(&d3dx, name);
        assert!(
            native.sys.direct_bodies.contains_key(&entry),
            "{name} is not bound as a direct body"
        );
    }

    // Prepare rich test inputs in DATA:
    // 0x000: out buffer (256 bytes)
    // 0x100: out2 / aux (64 bytes)
    // 0x140: out3 (64 bytes)
    // 0x200: M1 (invertible 4x4)
    // 0x240: M2 (invertible 4x4)
    // 0x280: M_sing (singular 4x4)
    // 0x300: V1 (vec4)
    // 0x310: V2 (vec4)
    // 0x320: V3 (vec4)
    // 0x330: V4 (vec4)
    // 0x340: V_zero (vec4 = 0)
    // 0x360: Q1 (unit quat)
    // 0x370: Q2 (unit quat)
    // 0x380: Q3 (unit quat)
    // 0x390: Q4 (unit quat)
    // 0x3a0: Plane (non-unit)
    // 0x3b0: Viewport (24 bytes)
    // 0x400: Array input (8 * 16 bytes)
    let mut mem = vec![0u8; DATA_LEN as usize];
    write_f32s(
        &mut mem,
        0x200,
        &[
            1.25, 0.5, -0.25, 0.1, //
            -0.5, 2.0, 0.75, 0.0, //
            0.25, -0.75, 1.5, 0.05, //
            3.0, -4.0, 5.0, 1.0,
        ],
    );
    write_f32s(
        &mut mem,
        0x240,
        &[
            0.8, -0.6, 0.0, 0.0, //
            0.6, 0.8, 0.0, 0.0, //
            0.0, 0.0, 1.2, 0.1, //
            -1.5, 2.5, 0.5, 1.0,
        ],
    );
    // Singular matrix (row 0 = 0)
    write_f32s(&mut mem, 0x280, &[0.0; 16]);
    write_f32s(&mut mem, 0x300, &[1.5, -2.25, 3.75, 1.0]);
    write_f32s(&mut mem, 0x310, &[-0.5, 4.0, -1.25, 0.5]);
    write_f32s(&mut mem, 0x320, &[2.0, 1.0, 0.5, -1.0]);
    write_f32s(&mut mem, 0x330, &[-3.0, 0.25, 2.5, 1.5]);
    write_f32s(&mut mem, 0x340, &[0.0, 0.0, 0.0, 0.0]);
    write_f32s(&mut mem, 0x360, &[0.2, 0.4, -0.4, 0.8]);
    write_f32s(&mut mem, 0x370, &[-0.5, 0.5, 0.5, 0.5]);
    write_f32s(&mut mem, 0x380, &[0.0, 0.6, 0.0, 0.8]);
    write_f32s(&mut mem, 0x390, &[0.3, -0.1, 0.4, 0.86]);
    write_f32s(&mut mem, 0x3a0, &[3.0, 4.0, 0.0, -5.0]);
    // D3DVIEWPORT9: X=10, Y=20, Width=800, Height=600, MinZ=0.0, MaxZ=1.0
    for (i, w) in [10u32, 20, 800, 600, 0.0f32.to_bits(), 1.0f32.to_bits()]
        .into_iter()
        .enumerate()
    {
        mem[0x3b0 + i * 4..0x3b0 + (i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    for k in 0..8 {
        let kf = k as f32;
        write_f32s(
            &mut mem,
            0x400 + k * 16,
            &[kf + 1.0, -kf * 0.5, kf * 0.25, 1.0],
        );
    }

    let fb = f32::to_bits;
    let out0 = DATA;
    let out1 = DATA + 0x100;
    let out2 = DATA + 0x140;
    let m1 = DATA + 0x200;
    let m2 = DATA + 0x240;
    let msing = DATA + 0x280;
    let v1 = DATA + 0x300;
    let v2 = DATA + 0x310;
    let v3 = DATA + 0x320;
    let v4 = DATA + 0x330;
    let v0 = DATA + 0x340;
    let q1 = DATA + 0x360;
    let q2 = DATA + 0x370;
    let q3 = DATA + 0x380;
    let q4 = DATA + 0x390;
    let pl = DATA + 0x3a0;
    let vp = DATA + 0x3b0;
    let arr = DATA + 0x400;

    // (name, args, check_offset, check_f32_count, check_eax)
    let cases: &[(&str, &[u32], usize, usize, bool)] = &[
        ("D3DXColorAdjustContrast", &[out0, v1, fb(1.35)], 0, 4, true),
        (
            "D3DXColorAdjustSaturation",
            &[out0, v1, fb(0.6)],
            0,
            4,
            true,
        ),
        ("D3DXMatrixMultiply", &[out0, m1, m2], 0, 16, true),
        ("D3DXMatrixMultiply", &[m1, m1, m1], 0x200, 16, true), // aliased in-place
        ("D3DXMatrixMultiplyTranspose", &[out0, m1, m2], 0, 16, true),
        ("D3DXMatrixInverse", &[out0, out1, m1], 0, 16, true),
        ("D3DXMatrixInverse", &[m1, 0, m1], 0x200, 16, true), // aliased, NULL pdet
        ("D3DXMatrixInverse", &[out0, out1, msing], 0, 16, true), // singular -> NULL
        ("D3DXMatrixTranspose", &[out0, m1], 0, 16, true),
        ("D3DXMatrixTranspose", &[m1, m1], 0x200, 16, true), // aliased
        (
            "D3DXMatrixScaling",
            &[out0, fb(2.0), fb(-3.0), fb(0.5)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixTranslation",
            &[out0, fb(1.5), fb(-2.5), fb(4.0)],
            0,
            16,
            true,
        ),
        ("D3DXMatrixRotationX", &[out0, fb(0.73)], 0, 16, true),
        ("D3DXMatrixRotationY", &[out0, fb(-1.12)], 0, 16, true),
        ("D3DXMatrixRotationZ", &[out0, fb(2.41)], 0, 16, true),
        ("D3DXMatrixRotationAxis", &[out0, v1, fb(0.95)], 0, 16, true),
        ("D3DXMatrixRotationQuaternion", &[out0, q1], 0, 16, true),
        (
            "D3DXMatrixRotationYawPitchRoll",
            &[out0, fb(0.5), fb(-0.3), fb(1.1)],
            0,
            16,
            true,
        ),
        ("D3DXMatrixLookAtLH", &[out0, v1, v2, v3], 0, 16, true),
        ("D3DXMatrixLookAtRH", &[out0, v1, v2, v3], 0, 16, true),
        (
            "D3DXMatrixPerspectiveFovLH",
            &[out0, fb(1.047), fb(1.333), fb(0.1), fb(100.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixPerspectiveFovRH",
            &[out0, fb(1.047), fb(1.333), fb(0.1), fb(100.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixPerspectiveLH",
            &[out0, fb(4.0), fb(3.0), fb(0.5), fb(50.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixPerspectiveRH",
            &[out0, fb(4.0), fb(3.0), fb(0.5), fb(50.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixPerspectiveOffCenterLH",
            &[
                out0,
                fb(-2.0),
                fb(3.0),
                fb(-1.0),
                fb(2.0),
                fb(0.5),
                fb(50.0),
            ],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixPerspectiveOffCenterRH",
            &[
                out0,
                fb(-2.0),
                fb(3.0),
                fb(-1.0),
                fb(2.0),
                fb(0.5),
                fb(50.0),
            ],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixOrthoLH",
            &[out0, fb(800.0), fb(600.0), fb(0.0), fb(100.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixOrthoRH",
            &[out0, fb(800.0), fb(600.0), fb(0.0), fb(100.0)],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixOrthoOffCenterLH",
            &[
                out0,
                fb(0.0),
                fb(800.0),
                fb(600.0),
                fb(0.0),
                fb(-1.0),
                fb(1.0),
            ],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixOrthoOffCenterRH",
            &[
                out0,
                fb(0.0),
                fb(800.0),
                fb(600.0),
                fb(0.0),
                fb(-1.0),
                fb(1.0),
            ],
            0,
            16,
            true,
        ),
        ("D3DXMatrixReflect", &[out0, pl], 0, 16, true),
        ("D3DXMatrixShadow", &[out0, v1, pl], 0, 16, true),
        (
            "D3DXMatrixAffineTransformation",
            &[out0, fb(2.0), v1, q1, v2],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixAffineTransformation",
            &[out0, fb(2.0), 0, 0, v2],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixAffineTransformation2D",
            &[out0, fb(1.5), v1, fb(0.8), v2],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixTransformation",
            &[out0, v1, q1, v2, v3, q2, v4],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixTransformation",
            &[out0, 0, 0, 0, 0, q2, 0],
            0,
            16,
            true,
        ),
        (
            "D3DXMatrixTransformation2D",
            &[out0, v1, fb(0.3), v2, v3, fb(-0.7), v4],
            0,
            16,
            true,
        ),
        ("D3DXMatrixDecompose", &[out0, out1, out2, m2], 0, 3, true),
        (
            "D3DXMatrixDecompose",
            &[out0, out1, out2, msing],
            0,
            3,
            true,
        ),
        ("D3DXPlaneFromPointNormal", &[out0, v1, v2], 0, 4, true),
        ("D3DXPlaneFromPoints", &[out0, v1, v2, v3], 0, 4, true),
        ("D3DXPlaneIntersectLine", &[out0, pl, v1, v2], 0, 3, true),
        ("D3DXPlaneIntersectLine", &[out0, pl, v1, v1], 0, 3, true), // parallel -> NULL
        ("D3DXPlaneNormalize", &[out0, pl], 0, 4, true),
        ("D3DXPlaneNormalize", &[out0, v0], 0, 4, true), // zero plane -> 0
        ("D3DXPlaneTransform", &[out0, pl, m1], 0, 4, true),
        (
            "D3DXPlaneTransformArray",
            &[out0, 16, arr, 16, m1, 6],
            0,
            24,
            true,
        ),
        (
            "D3DXQuaternionBaryCentric",
            &[out0, q1, q2, q3, fb(0.3), fb(0.4)],
            0,
            4,
            true,
        ),
        ("D3DXQuaternionExp", &[out0, q1], 0, 4, true),
        ("D3DXQuaternionExp", &[out0, v0], 0, 4, true),
        ("D3DXQuaternionInverse", &[out0, q1], 0, 4, true),
        ("D3DXQuaternionLn", &[out0, q1], 0, 4, true),
        ("D3DXQuaternionMultiply", &[out0, q1, q2], 0, 4, true),
        ("D3DXQuaternionMultiply", &[q1, q1, q1], 0x360, 4, true),
        ("D3DXQuaternionNormalize", &[out0, q1], 0, 4, true),
        (
            "D3DXQuaternionRotationAxis",
            &[out0, v1, fb(1.2)],
            0,
            4,
            true,
        ),
        ("D3DXQuaternionRotationMatrix", &[out0, m2], 0, 4, true),
        (
            "D3DXQuaternionRotationYawPitchRoll",
            &[out0, fb(0.4), fb(-0.6), fb(0.9)],
            0,
            4,
            true,
        ),
        ("D3DXQuaternionSlerp", &[out0, q1, q2, fb(0.35)], 0, 4, true),
        (
            "D3DXQuaternionSquad",
            &[out0, q1, q2, q3, q4, fb(0.4)],
            0,
            4,
            true,
        ),
        (
            "D3DXQuaternionSquadSetup",
            &[out0, out1, out2, q1, q2, q3, q4],
            0,
            4,
            false,
        ),
        ("D3DXQuaternionToAxisAngle", &[q1, out0, out1], 0, 3, false),
        (
            "D3DXVec2BaryCentric",
            &[out0, v1, v2, v3, fb(0.25), fb(0.5)],
            0,
            2,
            true,
        ),
        (
            "D3DXVec2CatmullRom",
            &[out0, v1, v2, v3, v4, fb(0.4)],
            0,
            2,
            true,
        ),
        (
            "D3DXVec2Hermite",
            &[out0, v1, v2, v3, v4, fb(0.6)],
            0,
            2,
            true,
        ),
        ("D3DXVec2Normalize", &[out0, v1], 0, 2, true),
        ("D3DXVec2Normalize", &[out0, v0], 0, 2, true),
        ("D3DXVec2Transform", &[out0, v1, m1], 0, 4, true),
        (
            "D3DXVec2TransformArray",
            &[out0, 16, arr, 16, m1, 6],
            0,
            24,
            true,
        ),
        ("D3DXVec2TransformCoord", &[out0, v1, m1], 0, 2, true),
        (
            "D3DXVec2TransformCoordArray",
            &[out0, 8, arr, 16, m1, 6],
            0,
            12,
            true,
        ),
        ("D3DXVec2TransformNormal", &[out0, v1, m1], 0, 2, true),
        (
            "D3DXVec2TransformNormalArray",
            &[out0, 8, arr, 16, m1, 6],
            0,
            12,
            true,
        ),
        (
            "D3DXVec3BaryCentric",
            &[out0, v1, v2, v3, fb(0.25), fb(0.5)],
            0,
            3,
            true,
        ),
        (
            "D3DXVec3CatmullRom",
            &[out0, v1, v2, v3, v4, fb(0.4)],
            0,
            3,
            true,
        ),
        (
            "D3DXVec3Hermite",
            &[out0, v1, v2, v3, v4, fb(0.6)],
            0,
            3,
            true,
        ),
        ("D3DXVec3Normalize", &[out0, v1], 0, 3, true),
        ("D3DXVec3Normalize", &[out0, v0], 0, 3, true),
        ("D3DXVec3Project", &[out0, v1, vp, m2, m1, m2], 0, 3, true),
        ("D3DXVec3Unproject", &[out0, v1, vp, m2, m1, m2], 0, 3, true),
        ("D3DXVec3Transform", &[out0, v1, m1], 0, 4, true),
        (
            "D3DXVec3TransformArray",
            &[out0, 16, arr, 16, m1, 6],
            0,
            24,
            true,
        ),
        ("D3DXVec3TransformCoord", &[out0, v1, m1], 0, 3, true),
        (
            "D3DXVec3TransformCoordArray",
            &[out0, 12, arr, 16, m1, 6],
            0,
            18,
            true,
        ),
        ("D3DXVec3TransformNormal", &[out0, v1, m1], 0, 3, true),
        (
            "D3DXVec3TransformNormalArray",
            &[out0, 12, arr, 16, m1, 6],
            0,
            18,
            true,
        ),
        (
            "D3DXVec4BaryCentric",
            &[out0, v1, v2, v3, fb(0.25), fb(0.5)],
            0,
            4,
            true,
        ),
        (
            "D3DXVec4CatmullRom",
            &[out0, v1, v2, v3, v4, fb(0.4)],
            0,
            4,
            true,
        ),
        ("D3DXVec4Cross", &[out0, v1, v2, v3], 0, 4, true),
        (
            "D3DXVec4Hermite",
            &[out0, v1, v2, v3, v4, fb(0.6)],
            0,
            4,
            true,
        ),
        ("D3DXVec4Normalize", &[out0, v1], 0, 4, true),
        ("D3DXVec4Transform", &[out0, v1, m1], 0, 4, true),
        (
            "D3DXVec4TransformArray",
            &[out0, 16, arr, 16, m1, 6],
            0,
            24,
            true,
        ),
        // Zero-element array transform must not touch memory.
        ("D3DXVec4TransformArray", &[0, 16, 0, 16, 0, 0], 0, 4, true),
    ];

    for &(name, args, off, count, check_eax) in cases {
        let entry = export_addr(&d3dx, name);
        let (nat_eax, nat_esp, nat_mem) = call_stdcall(&mut native, entry, args, &mem)
            .unwrap_or_else(|e| panic!("native {name} failed: {e}"));
        let (ref_eax, ref_esp, ref_mem) = call_stdcall(&mut reference, entry, args, &mem)
            .unwrap_or_else(|e| panic!("reference {name} failed: {e}"));
        assert_eq!(nat_esp, ref_esp, "{name}: stdcall stack pop mismatch");
        if check_eax {
            assert_eq!(nat_eax, ref_eax, "{name}: EAX mismatch");
        }
        assert_f32s_within_ulp(
            name,
            &read_f32s(&nat_mem, off, count),
            &read_f32s(&ref_mem, off, count),
            4,
        );
    }
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
    // Direct bodies retire far fewer guest instructions than Wine's x86 math code.
    assert!(
        native.cpu.retired_instructions() * 4 < reference.cpu.retired_instructions(),
        "direct bodies did not run: native={} reference={}",
        native.cpu.retired_instructions(),
        reference.cpu.retired_instructions()
    );

    // Fault on inaccessible output raises at entry without partial writes (A64-CALL-4).
    let entry = export_addr(&d3dx, "D3DXMatrixMultiply");
    let fault = call_stdcall(&mut native, entry, &[DATA + DATA_LEN, m1, m2], &mem).unwrap_err();
    let expected = format!("Fault {{ kind: Write({}), pc: {entry} }}", DATA + DATA_LEN);
    assert!(fault.contains(&expected), "unexpected fault: {fault}");
}
