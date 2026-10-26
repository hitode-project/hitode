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
fn leaf(f: &mut Fixture, name: &str, args: &[u32]) -> Option<u32> {
    let registry = f.m.sys.thunks.resolver_mut::<Registry>().unwrap();
    super::super::register(registry);
    let target = f.m.resolve_import("d3d9.dll", name).unwrap();
    let index = (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    f.m.cpu.set_reg(Reg::Esp, 0x550000);
    f.m.cpu.write_u32(0x550000, 0).unwrap();
    for (n, &arg) in args.iter().enumerate() {
        f.m.cpu.write_u32(0x550004 + n as u32 * 4, arg).unwrap();
    }
    let handler =
        f.m.sys
            .thunks
            .get(index)
            .f
            .env_leaf
            .expect("registered EnvLeaf");
    handler(&mut env(&mut f.m, index)).map(|v| v as u32)
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
fn binding_leaves_preserve_noops_switches_and_decline_final_releases_before_effects() {
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
        assert_eq!(leaf(&mut f, &name, &args(a)), Some(0), "{name}");
        let unchanged = versions(&mut f);
        assert_eq!(leaf(&mut f, &name, &args(a)), Some(0));
        assert_eq!(versions(&mut f), unchanged);
        assert_eq!(leaf(&mut f, &name, &args(b)), Some(0));
        assert_eq!(release(&mut env(&mut f.m, 0), b), 0);
        let unchanged = versions(&mut f);
        assert_eq!(leaf(&mut f, &name, &args(b)), Some(0));
        assert_eq!(leaf(&mut f, &name, &args(a)), None);
        assert_eq!(versions(&mut f), unchanged);
        assert!(
            com(&env(&mut f.m, 0))
                .get::<Payload>(&f.m.sys.objects, b)
                .is_some()
        );
        f.okay(&name, &args(a));
        assert!(
            com(&env(&mut f.m, 0))
                .get::<Payload>(&f.m.sys.objects, b)
                .is_none()
        );
    }
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0x44]),
        Some(0)
    );
    let unchanged = versions(&mut f);
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0x44]),
        Some(0)
    );
    assert_eq!(versions(&mut f), unchanged);
    // Recording the current value must still capture membership.
    f.okay("IDirect3DDevice9::BeginStateBlock", &[d]);
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0x44]),
        Some(0)
    );
    let output = f.packet(0u32);
    f.okay("IDirect3DDevice9::EndStateBlock", &[d, output]);
    let block = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetFVF", &[d, 0x104]);
    assert_eq!(
        leaf(&mut f, "IDirect3DStateBlock9::Apply", &[block]),
        Some(0)
    );
    assert_eq!(device(&env(&mut f.m, 0), d).unwrap().fixed.fvf, 0x44);
}

#[test]
// spec: GFX-REC-6
fn redundant_zero_fvf_leaf_keeps_the_unavailable_result() {
    let mut f = Fixture::new();
    let d = f.device;
    assert_eq!(device(&env(&mut f.m, 0), d).unwrap().fixed.fvf, 0);
    assert_ne!(leaf(&mut f, "IDirect3DDevice9::SetFVF", &[d, 0]), Some(0));
}

#[test]
// spec: GFX-REC-6, GFX-SHADER-4
fn state_block_leaves_admit_safe_transfers_and_decline_snapshot_destruction() {
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
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetVertexShader", &[d, a]),
        Some(0)
    );
    let output = f.packet(0u32);
    f.okay("IDirect3DDevice9::CreateStateBlock", &[d, 1, output]);
    let block = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(
        leaf(&mut f, "IDirect3DStateBlock9::Capture", &[block]),
        Some(0)
    );
    // A block equal to the device transfers nothing.
    let unchanged = versions(&mut f);
    for name in ["Capture", "Apply"] {
        let name = format!("IDirect3DStateBlock9::{name}");
        assert_eq!(leaf(&mut f, &name, &[block]), Some(0));
    }
    assert_eq!(versions(&mut f), unchanged);
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetVertexShader", &[d, b]),
        Some(0)
    );
    assert_eq!(
        leaf(&mut f, "IDirect3DStateBlock9::Apply", &[block]),
        Some(0)
    );
    assert_eq!(release(&mut env(&mut f.m, 0), a), 0);
    f.okay("IDirect3DDevice9::SetVertexShader", &[d, b]);
    let unchanged = versions(&mut f);
    assert_eq!(
        leaf(&mut f, "IDirect3DStateBlock9::Capture", &[block]),
        None
    );
    assert_eq!(versions(&mut f), unchanged);
    assert!(
        com(&env(&mut f.m, 0))
            .get::<Payload>(&f.m.sys.objects, a)
            .is_some()
    );
    f.okay("IDirect3DStateBlock9::Capture", &[block]);
    assert!(
        com(&env(&mut f.m, 0))
            .get::<Payload>(&f.m.sys.objects, a)
            .is_none()
    );
}

#[test]
// spec: GFX-REC-6
fn managed_texture_leaf_declines_before_dirty_upload_and_admits_resident_noop() {
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
    let before = versions(&mut f);
    let commands = f.m.ext.get::<State>().frames.recorded_commands;
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetTexture", &[d, 0, texture]),
        None
    );
    assert_eq!(versions(&mut f), before);
    assert_eq!(f.m.ext.get::<State>().frames.recorded_commands, commands);
    f.okay("IDirect3DDevice9::SetTexture", &[d, 0, texture]);
    assert_eq!(
        leaf(&mut f, "IDirect3DDevice9::SetTexture", &[d, 0, texture]),
        Some(0)
    );
}
