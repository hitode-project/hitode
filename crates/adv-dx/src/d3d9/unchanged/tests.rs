use super::*;
use crate::d3d9::stretch::tests::Fixture;
use adv_cpu::backend::{Guest, Reg};

fn env(m: &mut Machine<adv_cpu::JitCpu>, thunk: u32) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    }
}
/// Whether the check registered for `name` finds the call with `args`
/// changes nothing.
fn unchanged(f: &mut Fixture, name: &str, args: &[u32]) -> bool {
    f.m.cpu.set_reg(Reg::Esp, 0x550000);
    f.m.cpu.write_u32(0x550000, 0).unwrap();
    for (n, &arg) in args.iter().enumerate() {
        f.m.cpu.write_u32(0x550004 + n as u32 * 4, arg).unwrap();
    }
    let check = if name.starts_with("IDirect3DStateBlock9::") {
        state_block
    } else {
        binding
    };
    check(&mut env(&mut f.m, 0), name)
}
fn create(f: &mut Fixture, name: &str, mut args: Vec<u32>) -> u32 {
    let output = f.packet(0u32);
    args.push(output);
    if !name.contains("Shader") && !name.contains("Declaration") {
        args.push(0);
    }
    f.okay(name, &args);
    f.m.cpu.read_u32(output).unwrap()
}
fn versions(f: &mut Fixture) -> [u64; 15] {
    device(&env(&mut f.m, 0), f.device).unwrap().versions
}

#[test]
// spec: GFX-REC-6, GFX-SHADER-3, GFX-SHADER-4
fn rebinding_changes_nothing_and_switches_run_the_setter() {
    let mut f = Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let d = f.device;
    let vs = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0xffff,
    ]);
    let ps = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff]);
    let decl = f.packet([[0u8, 0, 0, 0, 2, 0, 0, 0], [255, 0, 0, 0, 17, 0, 0, 0]]);
    for (setter, creator, prefix, slot, suffix) in [
        (
            "SetTexture",
            "CreateTexture",
            vec![d, 2, 2, 1, 0, 21, 0],
            Some(0),
            vec![],
        ),
        (
            "SetStreamSource",
            "CreateVertexBuffer",
            vec![d, 64, 0, 0, 1],
            Some(2),
            vec![4, 12],
        ),
        (
            "SetIndices",
            "CreateIndexBuffer",
            vec![d, 12, 0, 101, 1],
            None,
            vec![],
        ),
        (
            "SetVertexShader",
            "CreateVertexShader",
            vec![d, vs],
            None,
            vec![],
        ),
        (
            "SetPixelShader",
            "CreatePixelShader",
            vec![d, ps],
            None,
            vec![],
        ),
        (
            "SetVertexDeclaration",
            "CreateVertexDeclaration",
            vec![d, decl],
            None,
            vec![],
        ),
    ] {
        let creator = format!("IDirect3DDevice9::{creator}");
        let a = create(&mut f, &creator, prefix.clone());
        let b = create(&mut f, &creator, prefix);
        let name = format!("IDirect3DDevice9::{setter}");
        let args = |pointer| {
            let mut a = vec![d];
            a.extend(slot);
            a.push(pointer);
            a.extend_from_slice(&suffix);
            a
        };
        assert!(!unchanged(&mut f, &name, &args(a)), "{name}");
        f.okay(&name, &args(a));
        assert!(unchanged(&mut f, &name, &args(a)), "{name}");
        let before = versions(&mut f);
        f.okay(&name, &args(a));
        assert_eq!(versions(&mut f), before);
        assert!(!unchanged(&mut f, &name, &args(b)));
        f.okay(&name, &args(b));
        assert_eq!(release(&mut env(&mut f.m, 0), b), 0);
        assert!(unchanged(&mut f, &name, &args(b)));
        // The switch releases the last reference through the setter.
        assert!(!unchanged(&mut f, &name, &args(a)));
        f.okay(&name, &args(a));
        assert!(
            com(&env(&mut f.m, 0))
                .get::<Payload>(&f.m.sys.objects, b)
                .is_none()
        );
    }
    f.okay("IDirect3DDevice9::SetFVF", &[d, 0x44]);
    assert!(unchanged(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0x44]));
    let before = versions(&mut f);
    f.okay("IDirect3DDevice9::SetFVF", &[d, 0x44]);
    assert_eq!(versions(&mut f), before);
    // Recording the current value must still capture membership.
    f.okay("IDirect3DDevice9::BeginStateBlock", &[d]);
    assert!(!unchanged(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0x44]));
    f.okay("IDirect3DDevice9::SetFVF", &[d, 0x44]);
    let output = f.packet(0u32);
    f.okay("IDirect3DDevice9::EndStateBlock", &[d, output]);
    let block = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetFVF", &[d, 0x104]);
    f.okay("IDirect3DStateBlock9::Apply", &[block]);
    assert_eq!(device(&env(&mut f.m, 0), d).unwrap().fixed.fvf, 0x44);
}

#[test]
fn redundant_zero_fvf_keeps_the_unavailable_result() {
    let mut f = Fixture::new();
    let d = f.device;
    assert_eq!(device(&env(&mut f.m, 0), d).unwrap().fixed.fvf, 0);
    assert!(!unchanged(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0]));
}

#[test]
// spec: GFX-SHADER-4
fn state_blocks_equal_to_the_device_transfer_nothing() {
    let mut f = Fixture::new();
    let d = f.device;
    let tokens = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0xffff,
    ]);
    let a = create(
        &mut f,
        "IDirect3DDevice9::CreateVertexShader",
        vec![d, tokens],
    );
    let b = create(
        &mut f,
        "IDirect3DDevice9::CreateVertexShader",
        vec![d, tokens],
    );
    f.okay("IDirect3DDevice9::SetVertexShader", &[d, a]);
    let output = f.packet(0u32);
    f.okay("IDirect3DDevice9::CreateStateBlock", &[d, 1, output]);
    let block = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DStateBlock9::Capture", &[block]);
    let before = versions(&mut f);
    for name in ["Capture", "Apply"] {
        let name = format!("IDirect3DStateBlock9::{name}");
        assert!(unchanged(&mut f, &name, &[block]), "{name}");
        f.okay(&name, &[block]);
    }
    assert_eq!(versions(&mut f), before);
    f.okay("IDirect3DDevice9::SetVertexShader", &[d, b]);
    assert!(!unchanged(&mut f, "IDirect3DStateBlock9::Apply", &[block]));
    f.okay("IDirect3DStateBlock9::Apply", &[block]);
    let bound = device(&env(&mut f.m, 0), d).unwrap().shaders.vertex;
    let a = com(&env(&mut f.m, 0)).identity::<Payload>(&f.m.sys.objects, a);
    assert_eq!(bound, a);
}

#[test]
fn managed_texture_rebinding_uploads_dirty_levels_and_skips_resident_ones() {
    let mut f = Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let d = f.device;
    let texture = create(
        &mut f,
        "IDirect3DDevice9::CreateTexture",
        vec![d, 2, 2, 1, 0, 21, 1],
    );
    assert!(!unchanged(
        &mut f,
        "IDirect3DDevice9::SetTexture",
        &[d, 0, texture]
    ));
    f.okay("IDirect3DDevice9::SetTexture", &[d, 0, texture]);
    assert!(unchanged(
        &mut f,
        "IDirect3DDevice9::SetTexture",
        &[d, 0, texture]
    ));
}
