//! Guest D3D9 sequences give the same results through direct host calls
//! as while traced.
use super::*;
use crate::d3d9::stretch::tests::Fixture;
use crate::d3d9::tests::{test_allocator, test_dc};
use adv_cpu::backend::{Guest, Reg, asm::Asm};
use adv_machine::{Conv, ObjectLimits, RunResult, Trace};
use std::time::{Duration, Instant};

const CODE: u32 = 0x400000;
const STACK: u32 = 0x550000;
type Snapshot = (u128, Vec<u128>, adv_cpu::ArchitecturalState, Vec<u8>);

#[derive(Default)]
struct Evidence {
    device: u32,
    ranges: Vec<(u32, usize)>,
    snapshots: Vec<Snapshot>,
    frames: Vec<adv_device::gpu::test_support::Frame>,
}
fn capture(env: &mut Env<'_>) -> Outcome {
    let pointer = env.ext.get::<Evidence>().device;
    let state = recording::draw_key(env, pointer);
    let keys = recording::command_keys(env, pointer);
    let cpu = env.cpu.capture_architecture().unwrap();
    let esp = env.cpu.reg(Reg::Esp);
    let mut memory = Vec::new();
    for &(address, len) in &env.ext.get::<Evidence>().ranges {
        let (address, len) = if address == STACK {
            (esp, (STACK + 4096 - esp) as usize)
        } else {
            (address, len)
        };
        let start = memory.len();
        memory.resize(start + len, 0);
        env.cpu.read(address, &mut memory[start..]).unwrap();
    }
    let evidence = env.ext.get_mut::<Evidence>();
    evidence.snapshots.push((state, keys, cpu, memory));
    // The second pass reuses these exact resource identities and thunk bytes.
    env.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
    env.sys.trace.all = true;
    Outcome::Return(0)
}
fn fixture(traced: bool) -> Fixture {
    let mut f = Fixture::with_native(ObjectLimits::default(), true);
    let state = f.m.ext.get_mut::<State>();
    state.provider.present_copy = |ext, surface, _, gpu| {
        let image = (ext.get::<State>().provider.target)(ext, surface)
            .unwrap()
            .image;
        let serial = ext.get::<Evidence>().frames.len() as u64 + 1;
        let frame = adv_device::gpu::test_support::present(gpu, &image, None, serial)?;
        ext.get_mut::<Evidence>().frames.push(frame);
        Ok(serial)
    };
    state.provider.present = |ext, surface, _, gpu| {
        let copy = ext.get::<State>().provider.present_copy;
        copy(ext, surface, None, gpu)
    };
    state.provider.presentation_serial = |_| None;
    let registry = f.m.sys.thunks.resolver_mut::<Registry>().unwrap();
    super::register(registry);
    registry.add("fixture.dll", "capture", Conv::Stdcall { argc: 0 }, capture);
    registry.add("fixture.dll", "next", Conv::Stdcall { argc: 0 }, |env| {
        Outcome::Return(u32::from(env.ext.get::<Evidence>().snapshots.len() < 2))
    });
    registry.add("fixture.dll", "exit", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    if traced {
        f.m.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
        f.m.sys.trace.all = true;
    }
    f.m.ext.insert(Evidence {
        device: f.device,
        ranges: vec![(STACK, 4096)],
        ..Default::default()
    });
    assert!(f.m.sys.set_initial_registers(
        0,
        &[
            (Reg::Esp, STACK + 0xf00),
            (Reg::Edx, 0x12345678),
            (Reg::Eflags, 0xed7)
        ]
    ));
    f
}
fn call(f: &mut Fixture, code: &mut Asm, name: &str, args: &[u32]) {
    let module = if name.contains("::") {
        "d3d9.dll"
    } else {
        "fixture.dll"
    };
    let target = f.m.resolve_import(module, name).unwrap();
    if module == "d3d9.dll"
        && let Some(body) = super::direct::lookup(name)
        && let Some(index) = f.m.sys.thunks.published_index(module, name)
    {
        f.m.sys.bind_thunk_body(index, body);
    }
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    let index = (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let slot = CODE + 0xc00 + index * 4;
    f.m.cpu.write_u32(slot, target).unwrap();
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&slot.to_le_bytes());
}
fn run(f: &mut Fixture, code: Asm) {
    f.m.cpu.write(CODE, &code.bytes).unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        match f.m.run() {
            RunResult::Exited(0) => break,
            RunResult::Idle { .. } => {
                super::pump(&mut f.m).unwrap();
                f.wake.wait(Some(Duration::from_millis(1))).unwrap();
            }
            result => panic!("D3D9 guest sequence: {result:?}"),
        }
        assert!(Instant::now() < until);
    }
}

// spec: A64-CTL-12, GFX-REC-1, GFX-REC-2
#[test]
fn d3d9_direct_and_traced_calls_match_state_command_keys_registers_stack_memory_and_present() {
    let mut f = fixture(false);
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let this = f.device;
    f.okay("IDirect3DDevice9::SetFVF", &[this, 0x44]);
    let vertices = f.packet([
        [0f32.to_bits(), 0, 0, 1f32.to_bits(), 0xffff0000],
        [16f32.to_bits(), 0, 0, 1f32.to_bits(), 0xff00ff00],
        [0, 12f32.to_bits(), 0, 1f32.to_bits(), 0xff0000ff],
    ]);
    let constants = f.packet([0x7fc12345u32, 0x80000000, 0x12345678, 0]);
    let output = f.packet([0u32; 4]);
    f.m.ext
        .get_mut::<Evidence>()
        .ranges
        .extend([(vertices, 60), (constants, 16), (output, 16)]);
    let mut code = Asm::new(CODE);
    let pass = code.here();
    code.bytes.extend_from_slice(&[0x31, 0xc0]); // xor eax, eax: identical incoming flags
    code.mov_ecx_imm(16);
    let top = code.here();
    for (name, args) in [
        ("SetRenderState", vec![this, 22, 1]),
        ("GetRenderState", vec![this, 22, output]),
        ("SetSamplerState", vec![this, 0, 5, 1]),
        ("SetTextureStageState", vec![this, 0, 1, 2]),
        ("SetVertexShaderConstantF", vec![this, 1, constants, 1]),
        ("GetVertexShaderConstantF", vec![this, 1, output, 1]),
        ("SetPixelShaderConstantI", vec![this, 2, constants, 1]),
        ("GetPixelShaderConstantI", vec![this, 2, output, 1]),
        ("SetPixelShaderConstantB", vec![this, 0, constants, 1]),
        ("Clear", vec![this, 0, 0, 1, 0xff123456, 1f32.to_bits(), 0]),
        ("BeginScene", vec![this]),
        ("DrawPrimitiveUP", vec![this, 4, 1, vertices, 20]),
        ("EndScene", vec![this]),
    ] {
        call(
            &mut f,
            &mut code,
            &format!("IDirect3DDevice9::{name}"),
            &args,
        );
    }
    code.dec_ecx().jnz(top);
    call(&mut f, &mut code, "capture", &[]);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::Present",
        &[this, 0, 0, 0, 0],
    );
    call(&mut f, &mut code, "next", &[]);
    code.bytes.extend_from_slice(&[0x85, 0xc0]); // test eax, eax
    code.jnz(pass);
    call(&mut f, &mut code, "exit", &[]);
    run(&mut f, code);
    let evidence = f.m.ext.get::<Evidence>();
    assert_eq!(evidence.snapshots.len(), 2);
    assert_eq!(evidence.snapshots[0].0, evidence.snapshots[1].0);
    assert_eq!(evidence.snapshots[0].1, evidence.snapshots[1].1);
    // Direct body call sites preserve DF and registers, and may change arithmetic flags (A64-CALL-3).
    const ARITHMETIC_FLAGS: u32 = 0x8d5;
    let mut direct_cpu = evidence.snapshots[0].2.clone();
    let mut traced_cpu = evidence.snapshots[1].2.clone();
    direct_cpu.eflags &= !ARITHMETIC_FLAGS;
    traced_cpu.eflags &= !ARITHMETIC_FLAGS;
    assert_eq!(direct_cpu, traced_cpu);
    let changes: Vec<_> = evidence.snapshots[0]
        .3
        .iter()
        .zip(&evidence.snapshots[1].3)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i, *a, *b))
        .collect();
    assert!(changes.is_empty(), "memory differences: {changes:?}");
    assert_eq!(evidence.snapshots[0].1.len(), 32);
    let images = evidence.frames.clone();
    assert_eq!(images.len(), 2);
    super::tests::flush_recordings(&mut Env {
        context: f.m.sys.current,
        cpu: &mut f.m.cpu,
        sys: &mut f.m.sys,
        ext: &mut f.m.ext,
        thunk: 0,
    });
    assert_eq!(
        images[0].pixels(f.m.ext.get_mut::<State>().gpu.as_mut().unwrap()),
        images[1].pixels(f.m.ext.get_mut::<State>().gpu.as_mut().unwrap())
    );
}

// spec: A64-CTL-12, GFX-REC-1
#[test]
fn last_recording_slot_flushes_alike_through_direct_and_traced_calls() {
    let mut direct = fixture(false);
    let mut traced = fixture(true);
    for f in [&mut direct, &mut traced] {
        let this = f.device;
        f.okay("IDirect3DDevice9::SetFVF", &[this, 0x44]);
        f.okay("IDirect3DDevice9::BeginScene", &[this]);
        let vertices = f.packet([
            [0f32.to_bits(), 0, 0, 1f32.to_bits(), 0xffff0000],
            [16f32.to_bits(), 0, 0, 1f32.to_bits(), 0xff00ff00],
            [0, 12f32.to_bits(), 0, 1f32.to_bits(), 0xff0000ff],
        ]);
        for _ in 0..4095 {
            f.okay(
                "IDirect3DDevice9::Clear",
                &[this, 0, 0, 1, 0xff123456, 1f32.to_bits(), 0],
            );
        }
        let mut code = Asm::new(CODE);
        code.mov_ecx_imm(16);
        let top = code.here();
        call(
            f,
            &mut code,
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[this, 0, 1, vertices, 20],
        );
        code.dec_ecx().jnz(top);
        call(
            f,
            &mut code,
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[this, 4, 1, vertices, 20],
        );
        call(f, &mut code, "exit", &[]);
        run(f, code);
    }
    assert_eq!(direct.m.ext.get::<State>().frames.recorded_commands, 4096);
    assert_eq!(
        direct.m.ext.get::<State>().frames.flushes[recording::Reason::Limit as usize],
        1
    );
    assert_eq!(
        direct.m.cpu.capture_architecture().unwrap(),
        traced.m.cpu.capture_architecture().unwrap()
    );
    let a = direct.image(direct.back);
    let b = traced.image(traced.back);
    assert_eq!(direct.pixels(&a), traced.pixels(&b));
}

fn com_call(code: &mut Asm, vtable_reg_base: u32, slot: u16, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    // mov eax, [vtable_reg_base] (address holds interface pointer); deref vtable and call [eax + slot * 4]
    code.bytes.push(0xa1); // mov eax, moffs32
    code.bytes.extend_from_slice(&vtable_reg_base.to_le_bytes());
    code.bytes.extend_from_slice(&[0x8b, 0x00]); // mov eax, [eax]
    code.bytes.extend_from_slice(&[0xff, 0x90]); // call dword ptr [eax + disp32]
    code.bytes
        .extend_from_slice(&(u32::from(slot) * 4).to_le_bytes());
}

fn com_slot(module: &str, method: &str) -> u16 {
    adv_abi::signature::com_signatures()
        .iter()
        .find(|s| s.module == module && s.name == method)
        .unwrap()
        .com_slot
        .unwrap()
}

// spec: A64-CALL-2, A64-CALL-3, A64-CALL-6, GFX-REC-1, GFX-REC-2
#[test]
fn all_bound_d3d9_direct_bodies_match_full_path_and_preserve_redundant_keys() {
    use adv_abi::types::D3DVIEWPORT9;

    #[derive(Default)]
    struct Hresults(Vec<u32>);

    let setup_pair = |traced: bool| -> (Fixture, u32, u32, u32, u32, u32, u32, u32) {
        let mut f = fixture(traced);
        f.m.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        {
            let registry = f.m.sys.thunks.resolver_mut::<Registry>().unwrap();
            registry.add(
                "fixture.dll",
                "record_hr",
                Conv::Stdcall { argc: 1 },
                |env| {
                    let esp = env.cpu.reg(Reg::Esp);
                    let hr = env.cpu.read_u32(esp + 4).unwrap();
                    env.ext.get_mut::<Hresults>().0.push(hr);
                    Outcome::Return(0)
                },
            );
        }
        f.m.ext.insert(Hresults::default());

        let call_out = |f: &mut Fixture, name: &str, args: &[u32]| -> u32 {
            let out = f.packet(0u32);
            let mut full = args.to_vec();
            full.push(out);
            f.okay(name, &full);
            f.m.cpu.read_u32(out).unwrap()
        };

        let this = f.device;
        let vb_out = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateVertexBuffer",
            &[this, 256, 0, 0, 0, vb_out, 0],
        );
        let vb = f.m.cpu.read_u32(vb_out).unwrap();
        let ib_out = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateIndexBuffer",
            &[this, 64, 0, 101, 0, ib_out, 0],
        );
        let ib = f.m.cpu.read_u32(ib_out).unwrap();
        let tex_out = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateTexture",
            &[this, 4, 4, 1, 0, 21, 0, tex_out, 0],
        );
        let tex = f.m.cpu.read_u32(tex_out).unwrap();
        let elements = f.packet([[0u8, 0, 0, 0, 2, 0, 0, 0], [0xff, 0, 0, 0, 17, 0, 0, 0]]);
        let decl = call_out(
            &mut f,
            "IDirect3DDevice9::CreateVertexDeclaration",
            &[this, elements],
        );
        let vs_tokens = f.packet([
            0xfffe_0200u32,
            0x0200_001f,
            0x8000_0000,
            0x900f_0000,
            0x0300_0002,
            0xc00f_0000,
            0x90e4_0000,
            0xa0e4_0003,
            0x0000_ffff,
        ]);
        let ps_tokens = f.packet([
            0xffff_0200u32,
            0x0200_0001,
            0x800f_0800,
            0xa0e4_0007,
            0x0000_ffff,
        ]);
        let vs = call_out(
            &mut f,
            "IDirect3DDevice9::CreateVertexShader",
            &[this, vs_tokens],
        );
        let ps = call_out(
            &mut f,
            "IDirect3DDevice9::CreatePixelShader",
            &[this, ps_tokens],
        );
        let sb = call_out(&mut f, "IDirect3DDevice9::CreateStateBlock", &[this, 1]);
        (f, vb, ib, tex, decl, vs, ps, sb)
    };

    let mut outs = Vec::new();
    let mut keys = Vec::new();
    let mut versions = Vec::new();
    let mut fingerprints = Vec::new();
    let mut results = Vec::new();

    for traced in [false, true] {
        let (mut f, vb, ib, tex, decl, vs, ps, sb) = setup_pair(traced);
        let this = f.device;
        let vp_ok = f.packet(D3DVIEWPORT9 {
            X: 1,
            Y: 2,
            Width: 8,
            Height: 6,
            MinZ: 0.25,
            MaxZ: 0.75,
        });
        let vp_bad = f.packet(D3DVIEWPORT9 {
            X: 0,
            Y: 0,
            Width: 0,
            Height: 6,
            MinZ: 0.0,
            MaxZ: 1.0,
        });
        let sc_ok = f.packet(RECT {
            left: 1,
            top: 1,
            right: 10,
            bottom: 8,
        });
        let sc_bad = f.packet(RECT {
            left: 5,
            top: 5,
            right: 4,
            bottom: 8,
        });
        let mat_a = f.packet([
            1f32, 0., 0., 0., 0., 2., 0., 0., 0., 0., 3., 0., 0.5, -0.25, 0., 1.,
        ]);
        let mat_b = f.packet([
            2f32, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 1., 2., 3., 1.,
        ]);
        let mat_nan = f.packet([f32::NAN; 16]);
        let plane_ok = f.packet([1f32, -2., 3., -4.]);
        let plane_nan = f.packet([1f32, f32::INFINITY, 0., 1.]);
        let f_in = f.packet([0x7fc1_2345u32, 0x8000_0000, 0x3f80_0000, 0xbf80_0000]);
        let i_in = f.packet([-1i32, 2, -3, 4]);
        let b_in = f.packet([1i32, 0, -5, 7]);
        let out_buf = f.packet([0u32; 64]);

        let mut code = Asm::new(CODE);
        let emit = |f: &mut Fixture, code: &mut Asm, method: &str, args: &[u32], rec: u32| {
            call(f, code, method, args);
            // push eax; call record_hr
            code.bytes.push(0x50);
            let target = f.m.resolve_import("fixture.dll", "record_hr").unwrap();
            let index =
                (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let slot = CODE + 0xc00 + index * 4;
            f.m.cpu.write_u32(slot, target).unwrap();
            code.bytes.extend_from_slice(&[0xff, 0x15]);
            code.bytes.extend_from_slice(&slot.to_le_bytes());
            let _ = rec;
        };

        let seq: Vec<(&str, Vec<u32>)> = vec![
            ("IDirect3DDevice9::BeginScene", vec![this]),
            ("IDirect3DDevice9::BeginScene", vec![this]), // invalid double BeginScene
            ("IDirect3DDevice9::SetRenderState", vec![this, 22, 1]),
            ("IDirect3DDevice9::SetRenderState", vec![this, 22, 1]), // redundant
            ("IDirect3DDevice9::SetRenderState", vec![this, 22, 99]), // invalid value
            ("IDirect3DDevice9::GetRenderState", vec![this, 22, out_buf]),
            (
                "IDirect3DDevice9::GetRenderState",
                vec![this, 999, out_buf + 4],
            ),
            ("IDirect3DDevice9::GetRenderState", vec![this, 22, 0x1000]), // unmapped ptr
            (
                "IDirect3DDevice9::SetTextureStageState",
                vec![this, 0, 1, 2],
            ),
            (
                "IDirect3DDevice9::SetTextureStageState",
                vec![this, 0, 1, 2],
            ), // redundant
            (
                "IDirect3DDevice9::SetTextureStageState",
                vec![this, 1, 7, 42],
            ),
            (
                "IDirect3DDevice9::SetTextureStageState",
                vec![this, 9, 1, 1],
            ), // invalid stage
            (
                "IDirect3DDevice9::GetTextureStageState",
                vec![this, 1, 7, out_buf + 8],
            ),
            ("IDirect3DDevice9::SetSamplerState", vec![this, 0, 5, 2]),
            ("IDirect3DDevice9::SetSamplerState", vec![this, 0, 5, 2]), // redundant
            (
                "IDirect3DDevice9::SetSamplerState",
                vec![this, 257, 8, 0x8000_0000],
            ), // -0.0
            (
                "IDirect3DDevice9::GetSamplerState",
                vec![this, 257, 8, out_buf + 12],
            ),
            ("IDirect3DDevice9::SetFVF", vec![this, 0]),
            ("IDirect3DDevice9::SetFVF", vec![this, 0x142]),
            ("IDirect3DDevice9::SetFVF", vec![this, 0x142]), // redundant
            ("IDirect3DDevice9::GetFVF", vec![this, out_buf + 16]),
            ("IDirect3DDevice9::SetViewport", vec![this, vp_ok]),
            ("IDirect3DDevice9::SetViewport", vec![this, vp_ok]), // redundant
            ("IDirect3DDevice9::SetViewport", vec![this, vp_bad]),
            ("IDirect3DDevice9::SetViewport", vec![this, 0x1000]), // unmapped
            ("IDirect3DDevice9::GetViewport", vec![this, out_buf + 20]),
            ("IDirect3DDevice9::SetScissorRect", vec![this, sc_ok]),
            ("IDirect3DDevice9::SetScissorRect", vec![this, sc_ok]), // redundant
            ("IDirect3DDevice9::SetScissorRect", vec![this, sc_bad]),
            ("IDirect3DDevice9::GetScissorRect", vec![this, out_buf + 44]),
            ("IDirect3DDevice9::SetTransform", vec![this, 256, mat_a]),
            ("IDirect3DDevice9::SetTransform", vec![this, 256, mat_a]), // redundant
            ("IDirect3DDevice9::SetTransform", vec![this, 999, 0x1000]), // unavailable before ptr read
            ("IDirect3DDevice9::SetTransform", vec![this, 2, mat_nan]),
            (
                "IDirect3DDevice9::MultiplyTransform",
                vec![this, 256, mat_b],
            ),
            (
                "IDirect3DDevice9::MultiplyTransform",
                vec![this, 999, mat_b],
            ),
            (
                "IDirect3DDevice9::GetTransform",
                vec![this, 256, out_buf + 60],
            ),
            ("IDirect3DDevice9::SetClipPlane", vec![this, 2, plane_ok]),
            ("IDirect3DDevice9::SetClipPlane", vec![this, 2, plane_ok]), // redundant
            ("IDirect3DDevice9::SetClipPlane", vec![this, 7, 0x1000]),   // index>=6 before read
            ("IDirect3DDevice9::SetClipPlane", vec![this, 2, plane_nan]),
            (
                "IDirect3DDevice9::GetClipPlane",
                vec![this, 2, out_buf + 124],
            ),
            (
                "IDirect3DDevice9::SetVertexShaderConstantF",
                vec![this, 4, f_in, 1],
            ),
            (
                "IDirect3DDevice9::SetVertexShaderConstantF",
                vec![this, 4, f_in, 1],
            ), // redundant
            (
                "IDirect3DDevice9::SetVertexShaderConstantF",
                vec![this, 0, 0, 0],
            ), // count=0
            (
                "IDirect3DDevice9::SetVertexShaderConstantF",
                vec![this, 256, f_in, 1],
            ), // OOB
            (
                "IDirect3DDevice9::SetVertexShaderConstantF",
                vec![this, 0, 0x1000, 1],
            ), // unmapped
            (
                "IDirect3DDevice9::GetVertexShaderConstantF",
                vec![this, 4, out_buf + 140, 1],
            ),
            (
                "IDirect3DDevice9::SetVertexShaderConstantI",
                vec![this, 2, i_in, 1],
            ),
            (
                "IDirect3DDevice9::GetVertexShaderConstantI",
                vec![this, 2, out_buf + 156, 1],
            ),
            (
                "IDirect3DDevice9::SetVertexShaderConstantB",
                vec![this, 1, b_in, 2],
            ),
            (
                "IDirect3DDevice9::GetVertexShaderConstantB",
                vec![this, 1, out_buf + 172, 2],
            ),
            (
                "IDirect3DDevice9::SetPixelShaderConstantF",
                vec![this, 3, f_in, 1],
            ),
            (
                "IDirect3DDevice9::GetPixelShaderConstantF",
                vec![this, 3, out_buf + 180, 1],
            ),
            (
                "IDirect3DDevice9::SetPixelShaderConstantI",
                vec![this, 1, i_in, 1],
            ),
            (
                "IDirect3DDevice9::GetPixelShaderConstantI",
                vec![this, 1, out_buf + 196, 1],
            ),
            (
                "IDirect3DDevice9::SetPixelShaderConstantB",
                vec![this, 3, b_in, 1],
            ),
            (
                "IDirect3DDevice9::GetPixelShaderConstantB",
                vec![this, 3, out_buf + 212, 1],
            ),
            ("IDirect3DDevice9::SetVertexShader", vec![this, 0]), // redundant null
            ("IDirect3DDevice9::SetVertexShader", vec![this, vs]),
            ("IDirect3DDevice9::SetVertexShader", vec![this, vs]), // redundant bound
            ("IDirect3DDevice9::SetPixelShader", vec![this, ps]),
            ("IDirect3DDevice9::SetPixelShader", vec![this, ps]), // redundant bound
            ("IDirect3DDevice9::SetVertexDeclaration", vec![this, decl]),
            ("IDirect3DDevice9::SetVertexDeclaration", vec![this, decl]), // redundant bound
            ("IDirect3DDevice9::SetFVF", vec![this, 0x44]),               // unbinds decl!
            ("IDirect3DDevice9::SetStreamSource", vec![this, 0, 0, 0, 0]), // redundant null
            (
                "IDirect3DDevice9::SetStreamSource",
                vec![this, 0, vb, 16, 32],
            ),
            (
                "IDirect3DDevice9::SetStreamSource",
                vec![this, 0, vb, 16, 32],
            ), // redundant
            (
                "IDirect3DDevice9::SetStreamSource",
                vec![this, 0, vb, 32, 16],
            ), // same VB new off/stride
            (
                "IDirect3DDevice9::SetStreamSource",
                vec![this, 0, vb, 999, 16],
            ), // offset > len
            (
                "IDirect3DDevice9::SetStreamSourceFreq",
                vec![this, 0, 0x4000_0002],
            ),
            (
                "IDirect3DDevice9::SetStreamSourceFreq",
                vec![this, 0, 0x4000_0002],
            ),
            (
                "IDirect3DDevice9::GetStreamSourceFreq",
                vec![this, 0, out_buf + 216],
            ),
            ("IDirect3DDevice9::SetIndices", vec![this, 0]),
            ("IDirect3DDevice9::SetIndices", vec![this, ib]),
            ("IDirect3DDevice9::SetIndices", vec![this, ib]), // redundant
            ("IDirect3DDevice9::SetTexture", vec![this, 0, 0]),
            ("IDirect3DDevice9::SetTexture", vec![this, 0, tex]),
            ("IDirect3DDevice9::SetTexture", vec![this, 0, tex]), // redundant clean
            ("IDirect3DStateBlock9::Capture", vec![sb]),
            ("IDirect3DStateBlock9::Apply", vec![sb]),
            ("IDirect3DStateBlock9::Apply", vec![sb]), // unchanged apply
            ("IDirect3DDevice9::SetRenderState", vec![this, 22, 2]),
            ("IDirect3DStateBlock9::Apply", vec![sb]), // dirty apply restores 22
            ("IDirect3DDevice9::EndScene", vec![this]),
            ("IDirect3DDevice9::EndScene", vec![this]), // invalid second EndScene
        ];
        for (method, args) in &seq {
            emit(&mut f, &mut code, method, args, 0);
        }
        call(&mut f, &mut code, "exit", &[]);
        run(&mut f, code);

        let mut mem = vec![0u8; 256];
        f.m.cpu.read(out_buf, &mut mem).unwrap();
        outs.push(mem);
        results.push(f.m.ext.get::<Hresults>().0.clone());
        let env_now = Env {
            context: f.m.sys.current,
            cpu: &mut f.m.cpu,
            sys: &mut f.m.sys,
            ext: &mut f.m.ext,
            thunk: 0,
        };
        let d = device(&env_now, this).unwrap();
        let id_of = |ptr: u32| com(&env_now).identity::<Payload>(&env_now.sys.objects, ptr);
        let hot = d.hot.borrow();
        assert_eq!(hot.shaders.vertex, id_of(vs));
        assert_eq!(hot.shaders.pixel, id_of(ps));
        assert_eq!(hot.buffers.declaration, None);
        assert_eq!(hot.buffers.streams[0].buffer, id_of(vb));
        assert_eq!(hot.buffers.streams[0].offset, 32);
        assert_eq!(hot.buffers.streams[0].stride, 16);
        assert_eq!(hot.buffers.frequencies[0], 0x4000_0002);
        assert_eq!(hot.buffers.indices, id_of(ib));
        assert_eq!(hot.texture, id_of(tex));
        assert_eq!(hot.shaders.textures[0], None);
        keys.push(hot.fixed.key());
        versions.push(hot.versions);
        fingerprints.push(hot.fingerprints);
    }

    assert_eq!(results[0], results[1]);
    assert_eq!(outs[0], outs[1]);
    assert_eq!(keys[0], keys[1]);
    assert_eq!(versions[0], versions[1]);
    // Classes 8..=12 hash per-Fixture ObjectIds; all value/constant classes (0..=7, 13..=14)
    // must match bit-for-bit across direct and traced paths.
    assert_eq!(fingerprints[0][..8], fingerprints[1][..8]);
    assert_eq!(fingerprints[0][13..], fingerprints[1][13..]);

    // Verify redundant sets preserve CachedFixed::key and versions on direct path.
    let mut f = fixture(false);
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let this = f.device;
    f.okay("IDirect3DDevice9::SetRenderState", &[this, 22, 1]);
    let (v0, key0) = {
        let e = Env {
            context: f.m.sys.current,
            cpu: &mut f.m.cpu,
            sys: &mut f.m.sys,
            ext: &mut f.m.ext,
            thunk: 0,
        };
        let d = device(&e, this).unwrap();
        let hot = d.hot.borrow();
        (hot.versions, hot.fixed.key())
    };
    let mut code = Asm::new(CODE);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetRenderState",
        &[this, 22, 1],
    );
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetSamplerState",
        &[this, 0, 5, 1],
    );
    call(&mut f, &mut code, "exit", &[]);
    run(&mut f, code);
    let e = Env {
        context: f.m.sys.current,
        cpu: &mut f.m.cpu,
        sys: &mut f.m.sys,
        ext: &mut f.m.ext,
        thunk: 0,
    };
    let d = device(&e, this).unwrap();
    let hot = d.hot.borrow();
    assert_eq!(hot.versions, v0);
    assert_eq!(hot.fixed.key(), key0);
}

// spec: A64-CALL-2, A64-CALL-6, GFX-REC-1, GFX-REC-2
#[test]
fn d3d9_direct_bodies_fall_back_for_stateblock_recording_dirty_managed_texture_reset_and_release() {
    let mut f = fixture(false);
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let this = f.device;
    let tex_out = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateTexture",
        &[this, 2, 2, 1, 0, 21, 1, tex_out, 0], // Managed texture (Pool = 1)
    );
    let tex = f.m.cpu.read_u32(tex_out).unwrap();
    let lock_out = f.packet([0u32; 2]);
    f.okay("IDirect3DTexture9::LockRect", &[tex, 0, lock_out, 0, 0]);
    let locked = GuestPtr::<adv_abi::types::D3DLOCKED_RECT>::new(lock_out)
        .read(&f.m.cpu)
        .unwrap();
    f.m.cpu.write_u32(locked.pBits, 0xff11_2233).unwrap();
    f.okay("IDirect3DTexture9::UnlockRect", &[tex, 0]);
    f.okay("IDirect3DDevice9::SetTexture", &[this, 0, tex]);

    // Dirty an ALREADY bound managed texture via LockRect, record a state block,
    // apply it, flush the dirty texture via direct SetTexture, release, reset,
    // and finally exercise a direct call against the released device.
    f.okay("IDirect3DTexture9::LockRect", &[tex, 0, lock_out, 0, 0]);
    let locked = GuestPtr::<adv_abi::types::D3DLOCKED_RECT>::new(lock_out)
        .read(&f.m.cpu)
        .unwrap();
    f.m.cpu.write_u32(locked.pBits, 0xff44_5566).unwrap();

    #[derive(Default)]
    struct FallbackChecks {
        device: u32,
        tex: u32,
        tex_img: Option<adv_device::gpu::Image>,
        stale_hr: Option<u32>,
    }
    f.m.ext.insert(FallbackChecks {
        device: this,
        tex,
        ..Default::default()
    });
    {
        let registry = f.m.sys.thunks.resolver_mut::<Registry>().unwrap();
        registry.add(
            "fixture.dll",
            "check_recording",
            Conv::Stdcall { argc: 0 },
            |env| {
                let this = env.ext.get::<FallbackChecks>().device;
                let d = device(env, this).unwrap();
                assert_eq!(d.hot.borrow().fixed.render[&22], 3);
                assert_eq!(d.hot.borrow().shaders.vertex_constants.floats[2], [0.; 4]);
                Outcome::Return(0)
            },
        );
        registry.add(
            "fixture.dll",
            "check_applied",
            Conv::Stdcall { argc: 0 },
            |env| {
                let this = env.ext.get::<FallbackChecks>().device;
                let d = device(env, this).unwrap();
                assert_eq!(d.hot.borrow().fixed.render[&22], 1);
                assert_eq!(
                    d.hot.borrow().shaders.vertex_constants.floats[2],
                    [0.5, 0.25, 0.125, 1.0]
                );
                Outcome::Return(0)
            },
        );
        registry.add(
            "fixture.dll",
            "capture_tex_image",
            Conv::Stdcall { argc: 0 },
            |env| {
                let tex = env.ext.get::<FallbackChecks>().tex;
                let r = resources::resource(env, tex).unwrap();
                let target =
                    (env.ext.get::<State>().provider.target)(env.ext, r.surface.unwrap()).unwrap();
                env.ext.get_mut::<FallbackChecks>().tex_img = Some(target.image);
                Outcome::Return(0)
            },
        );
        registry.add(
            "fixture.dll",
            "check_reset",
            Conv::Stdcall { argc: 0 },
            |env| {
                let this = env.ext.get::<FallbackChecks>().device;
                let d = device(env, this).unwrap();
                assert_eq!(d.hot.borrow().fixed.render[&22], 2);
                assert!(d.hot.borrow().texture.is_none());
                Outcome::Return(0)
            },
        );
        registry.add(
            "fixture.dll",
            "record_stale",
            Conv::Stdcall { argc: 1 },
            |env| {
                let esp = env.cpu.reg(Reg::Esp);
                let hr = env.cpu.read_u32(esp + 4).unwrap();
                env.ext.get_mut::<FallbackChecks>().stale_hr = Some(hr);
                Outcome::Return(0)
            },
        );
    }

    let f_in = f.packet([0.5f32, 0.25, 0.125, 1.0]);
    let sb_out = f.packet(0u32);
    let mut pp = adv_abi::types::D3DPRESENT_PARAMETERS {
        BackBufferWidth: 16,
        BackBufferHeight: 12,
        BackBufferFormat: 22,
        BackBufferCount: 1,
        SwapEffect: 1,
        hDeviceWindow: 0x7788,
        Windowed: 1,
        ..adv_abi::types::D3DPRESENT_PARAMETERS::default()
    };
    let pp_ptr = f.packet(pp);
    let _ = &mut pp;

    f.okay("IDirect3DDevice9::BeginStateBlock", &[this]);
    let mut code = Asm::new(CODE);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetRenderState",
        &[this, 22, 1],
    );
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetVertexShaderConstantF",
        &[this, 2, f_in, 1],
    );
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetTexture",
        &[this, 0, tex],
    );
    call(&mut f, &mut code, "check_recording", &[]);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::EndStateBlock",
        &[this, sb_out],
    );
    // Apply stateblock (`push dword ptr [sb_out]`; `call Apply`).
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&sb_out.to_le_bytes());
    let apply_t =
        f.m.resolve_import("d3d9.dll", "IDirect3DStateBlock9::Apply")
            .unwrap();
    if let Some(body) = super::direct::lookup("IDirect3DStateBlock9::Apply")
        && let Some(idx) =
            f.m.sys
                .thunks
                .published_index("d3d9.dll", "IDirect3DStateBlock9::Apply")
    {
        f.m.sys.bind_thunk_body(idx, body);
    }
    let apply_idx = (apply_t - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let apply_slot = CODE + 0xc00 + apply_idx * 4;
    f.m.cpu.write_u32(apply_slot, apply_t).unwrap();
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&apply_slot.to_le_bytes());
    call(&mut f, &mut code, "check_applied", &[]);

    // Unlock managed texture and re-bind via direct call: must hit Stop::Guest and flush 0xff44_5566.
    call(
        &mut f,
        &mut code,
        "IDirect3DTexture9::UnlockRect",
        &[tex, 0],
    );
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetTexture",
        &[this, 0, tex],
    );
    call(&mut f, &mut code, "capture_tex_image", &[]);

    // Release stateblock (`push dword ptr [sb_out]`; `call Release`) and texture, then Reset.
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&sb_out.to_le_bytes());
    let rel_t =
        f.m.resolve_import("d3d9.dll", "IDirect3DStateBlock9::Release")
            .unwrap();
    let rel_idx = (rel_t - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let rel_slot = CODE + 0xc00 + rel_idx * 4;
    f.m.cpu.write_u32(rel_slot, rel_t).unwrap();
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&rel_slot.to_le_bytes());
    call(&mut f, &mut code, "IDirect3DTexture9::Release", &[tex]);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::Reset",
        &[this, pp_ptr],
    );
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetRenderState",
        &[this, 22, 2],
    );
    call(&mut f, &mut code, "check_reset", &[]);

    // Release the device itself, then issue a direct body on the stale interface pointer.
    call(&mut f, &mut code, "IDirect3DDevice9::Release", &[this]);
    call(
        &mut f,
        &mut code,
        "IDirect3DDevice9::SetRenderState",
        &[this, 22, 1],
    );
    code.bytes.push(0x50); // push eax
    let rec_t = f.m.resolve_import("fixture.dll", "record_stale").unwrap();
    let rec_idx = (rec_t - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let rec_slot = CODE + 0xc00 + rec_idx * 4;
    f.m.cpu.write_u32(rec_slot, rec_t).unwrap();
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&rec_slot.to_le_bytes());
    call(&mut f, &mut code, "exit", &[]);

    run(&mut f, code);
    let tex_img = f.m.ext.get::<FallbackChecks>().tex_img.clone().unwrap();
    assert_eq!(f.m.ext.get::<FallbackChecks>().stale_hr, Some(INVALID));
    let mut transfer =
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&tex_img, None)
            .unwrap();
    transfer.take_completion_job().unwrap()();
    assert_eq!(
        transfer.poll().unwrap().unwrap()[..4],
        [0x66, 0x55, 0x44, 0xff]
    );
}

// spec: A64-CALL-2, A64-CALL-5
#[test]
fn d3d9_direct_body_loop_keeps_host_calls_flat_on_real_vtable() {
    use adv_abi::types::D3DPRESENT_PARAMETERS;
    #[derive(Default)]
    struct Surfaces {
        next: u32,
        targets: BTreeMap<u32, Target>,
    }
    #[derive(Default)]
    struct CallLog {
        samples: Vec<u64>,
    }

    let cpu = adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap();
    let mut m = Machine::new(cpu, Registry::default(), adv_machine::Clock::virtual_time()).unwrap();
    crate::test_support::install(&mut m);
    let wake = std::sync::Arc::new(adv_sync::WakeEvent::new().unwrap());
    let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
    let sender = builder.sender(8, 8).unwrap();
    let _service = builder.start().unwrap();
    let provider = SurfaceProvider {
        allocator: test_allocator(),
        dc: test_dc(),
        insert: |ext, target| {
            let s = ext.get_mut::<Surfaces>();
            s.next += 1;
            s.targets.insert(s.next, target);
            Ok(s.next)
        },
        cpu_access: |ext, id, rect| {
            let target = (ext.get::<State>().provider.target)(ext, id)?;
            Some(CpuReadback::new(target.image, rect))
        },
        gpu_access: |env, id, _, _| (env.ext.get::<State>().provider.target)(env.ext, id),
        target: |ext, surface| ext.get::<Surfaces>().targets.get(&surface).cloned(),
        remove: |ext, _, surface| {
            ext.get_mut::<Surfaces>().targets.remove(&surface);
            Ok(())
        },
        replace: |ext, surface, target| {
            *ext.get_mut::<Surfaces>().targets.get_mut(&surface).unwrap() = target;
            Ok(())
        },
        present: |_, _, _, _| Ok(0),
        present_copy: |_, _, _, _| Ok(0),
        repeat: |_, _| Ok(0),
        presentation_serial: |_| Some(0),
        vsync: |_| None,
        guest_frame_wait: |_| false,
        window: |_, _, _| Some((16, 12)),
        visible: |_, _, _, _| Some(false),
        window_presented: |_, _| {},
        fast_forward: |_| false,
        display: |_| (16, 12, 60),
        monitor: 0x0001_0001,
        mode_changed: |_, _, _| Outcome::Return(0),
        device_released: |_, _| {},
        completed: |_, _| None,
    };
    {
        let registry = m.sys.thunks.resolver_mut::<Registry>().unwrap();
        super::register(registry);
        registry.add(
            "fixture.dll",
            "mark_calls",
            Conv::Stdcall { argc: 0 },
            |env| {
                let calls = env.sys.host_calls;
                env.ext.get_mut::<CallLog>().samples.push(calls);
                Outcome::Return(0)
            },
        );
        registry.add("fixture.dll", "exit", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
    }
    super::install(&mut m, provider).unwrap();
    m.ext.insert(Surfaces::default());
    m.ext.insert(CallLog::default());
    let gpu = adv_device::gpu::test_support::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    )))
    .unwrap();
    super::set_gpu(&mut m.ext, gpu).unwrap();
    {
        let s = m.ext.get_mut::<State>();
        s.sender = Some(std::rc::Rc::new(std::cell::RefCell::new(sender)));
        s.wake = Some(wake);
    }

    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    let (factory, _) = object(
        &mut Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        },
        "IDirect3D9",
        IID_FACTORY,
        Payload::Factory,
    )
    .unwrap();
    let pp_ptr = data;
    let dev_slot = data + 128;
    let vb_slot = data + 132;
    let c_ptr = data + 256;
    let mat_ptr = data + 320;
    GuestPtr::<D3DPRESENT_PARAMETERS>::new(pp_ptr)
        .write(
            &mut m.cpu,
            D3DPRESENT_PARAMETERS {
                BackBufferWidth: 16,
                BackBufferHeight: 12,
                BackBufferFormat: 22,
                BackBufferCount: 1,
                SwapEffect: 1,
                hDeviceWindow: 0x7788,
                Windowed: 1,
                ..D3DPRESENT_PARAMETERS::default()
            },
        )
        .unwrap();
    GuestPtr::<[f32; 4]>::new(c_ptr)
        .write(&mut m.cpu, [1.0, 2.0, 3.0, 4.0])
        .unwrap();
    GuestPtr::<[f32; 16]>::new(mat_ptr)
        .write(&mut m.cpu, [1.0; 16])
        .unwrap();

    // Create device and VB via host handlers first.
    let call_host = |m: &mut Machine<adv_cpu::JitCpu>, name: &str, args: &[u32]| -> u32 {
        m.cpu.set_reg(Reg::Esp, STACK + 0xf00);
        m.cpu.write_u32(STACK + 0xf00, 0).unwrap();
        for (i, &a) in args.iter().enumerate() {
            m.cpu
                .write_u32(STACK + 0xf00 + 4 + i as u32 * 4, a)
                .unwrap();
        }
        let handler = m
            .sys
            .thunks
            .resolver_mut::<Registry>()
            .unwrap()
            .get("d3d9.dll", name)
            .unwrap()
            .handler;
        let mut e = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let Outcome::Return(r) = handler(&mut e) else {
            panic!("expected immediate return from {name}");
        };
        r
    };
    assert_eq!(
        call_host(
            &mut m,
            "IDirect3D9::CreateDevice",
            &[factory, 0, 1, 0x7788, 0x22, pp_ptr, dev_slot],
        ),
        0
    );
    let dev = m.cpu.read_u32(dev_slot).unwrap();
    assert_eq!(
        call_host(
            &mut m,
            "IDirect3DDevice9::CreateVertexBuffer",
            &[dev, 256, 0, 0, 0, vb_slot, 0],
        ),
        0
    );
    let vb = m.cpu.read_u32(vb_slot).unwrap();

    // Bind vb at stream 0 once before loop.
    m.cpu.write_u32(dev_slot, dev).unwrap();
    let mut code = Asm::new(CODE);
    // Warmup iteration (offset 0 -> 16) + mark_calls, then 2000-iteration loop + mark_calls + exit.
    let emit_body_set = |code: &mut Asm| {
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "BeginScene"),
            &[dev],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetRenderState"),
            &[dev, 22, 1],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetTextureStageState"),
            &[dev, 0, 1, 2],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetSamplerState"),
            &[dev, 0, 5, 1],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetTransform"),
            &[dev, 256, mat_ptr],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetVertexShaderConstantF"),
            &[dev, 0, c_ptr, 1],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetPixelShaderConstantF"),
            &[dev, 0, c_ptr, 1],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetStreamSource"),
            &[dev, 0, vb, 16, 16],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "SetTexture"),
            &[dev, 0, 0],
        );
        com_call(
            code,
            dev_slot,
            com_slot("IDirect3DDevice9", "EndScene"),
            &[dev],
        );
    };
    // Initial SetStreamSource to bind vb via host call, then sample after warmup
    // iterations so compiled traces run all 10 COM vtable calls per iteration.
    com_call(
        &mut code,
        dev_slot,
        com_slot("IDirect3DDevice9", "SetStreamSource"),
        &[dev, 0, vb, 0, 16],
    );
    let mark_t = m.resolve_import("fixture.dll", "mark_calls").unwrap();
    let mark_slot = data + 400;
    m.cpu.write_u32(mark_slot, mark_t).unwrap();

    code.mov_ecx_imm(2000);
    let loop_top = code.here();
    code.bytes.push(0x51); // push ecx (preserve across calls if needed)
    // Sample host_calls once after 5 warmup iterations (when ecx == 1995).
    code.bytes.extend_from_slice(&[0x81, 0xf9]); // cmp ecx, 1995
    code.bytes.extend_from_slice(&1995u32.to_le_bytes());
    let jne_patch = code.here();
    code.bytes.extend_from_slice(&[0x75, 0x00]); // jne rel8
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&mark_slot.to_le_bytes());
    let after_mark = code.here();
    code.bytes[(jne_patch - CODE) as usize + 1] = (after_mark - (jne_patch + 2)) as u8;
    emit_body_set(&mut code);
    code.bytes.push(0x59); // pop ecx
    code.dec_ecx().jnz(loop_top);

    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&mark_slot.to_le_bytes());
    let exit_t = m.resolve_import("fixture.dll", "exit").unwrap();
    let exit_slot = data + 404;
    m.cpu.write_u32(exit_slot, exit_t).unwrap();
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&exit_slot.to_le_bytes());

    m.cpu.write(CODE, &code.bytes).unwrap();
    m.sys
        .set_initial_registers(0, &[(Reg::Esp, STACK + 0xf00), (Reg::Eflags, 0x202)]);
    m.spawn("direct loop", CODE, &[], 0x10000).unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        match m.run() {
            RunResult::Exited(0) => break,
            RunResult::Idle { .. } => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other => panic!("unexpected run result: {other:?}"),
        }
        assert!(Instant::now() < until);
    }

    let samples = &m.ext.get::<CallLog>().samples;
    assert_eq!(samples.len(), 2);
    // Only the single mark_calls trap itself ran between warmup and loop end!
    assert_eq!(
        samples[1] - samples[0],
        1,
        "2000 iterations of 10 COM vtable calls must add zero d3d9 host traps (got {} -> {})",
        samples[0],
        samples[1]
    );
}

// spec: GFX-REC-6
#[test]
fn direct_unavailable_options_are_reported_once_by_the_full_handler() {
    const WARM: u32 = 64;
    const ROUNDS: u32 = 80;
    #[derive(Default)]
    struct Options {
        warm: u32,
        slots: [u32; 3],
    }
    let mut f = fixture(false);
    let this = f.device;
    let matrix = f.packet([1f32.to_bits(); 16]);
    let results = f.packet([0u32; 3]);
    let counter = f.packet(ROUNDS);
    // Supported values warm the call sites onto their direct bodies first.
    let slots = [f.packet(4u32), f.packet(2u32), f.packet(256u32)];
    f.m.ext.insert(Options { warm: WARM, slots });
    let registry = f.m.sys.thunks.resolver_mut::<Registry>().unwrap();
    registry.add("fixture.dll", "flip", Conv::Stdcall { argc: 0 }, |env| {
        let options = env.ext.get_mut::<Options>();
        options.warm = options.warm.saturating_sub(1);
        if options.warm == 0 {
            // Stage-zero ADDSIGNED, a TEMP argument and a texture matrix are
            // legal SDK options this device does not implement.
            let slots = options.slots;
            for (slot, value) in slots.into_iter().zip([8, 5, 16]) {
                env.cpu.write_u32(slot, value).unwrap();
            }
        }
        Outcome::Return(0)
    });
    let mut code = Asm::new(CODE);
    let top = code.here();
    call(&mut f, &mut code, "flip", &[]);
    for (index, (name, leading)) in [
        ("SetTextureStageState", vec![this, 0, 1]),
        ("SetTextureStageState", vec![this, 0, 2]),
        ("SetTransform", vec![this]),
    ]
    .into_iter()
    .enumerate()
    {
        // The last argument comes from its slot: the option, or the matrix
        // after the transform state.
        if index == 2 {
            code.push_imm(matrix);
        }
        code.bytes.extend_from_slice(&[0xff, 0x35]); // push dword [slot]
        code.bytes.extend_from_slice(&slots[index].to_le_bytes());
        for &arg in leading.iter().rev() {
            code.push_imm(arg);
        }
        call(&mut f, &mut code, &format!("IDirect3DDevice9::{name}"), &[]);
        code.bytes.push(0xa3); // mov [moffs32], eax
        code.bytes
            .extend_from_slice(&(results + index as u32 * 4).to_le_bytes());
    }
    code.bytes.extend_from_slice(&[0xff, 0x0d]); // dec dword [counter]
    code.bytes.extend_from_slice(&counter.to_le_bytes());
    code.jnz(top);
    call(&mut f, &mut code, "exit", &[]);
    run(&mut f, code);
    let returned: [u32; 3] = GuestPtr::new(results).read(&f.m.cpu).unwrap();
    assert_eq!(returned, [UNAVAILABLE; 3]);
    let diagnostics = f.m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 3);
    assert_eq!(diagnostics.warnings.values().sum::<u64>(), 3);
}
