//! Guest D3D9 sequences give the same results through direct host calls
//! as while traced.
use super::*;
use crate::d3d9::stretch::tests::Fixture;
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
    frames: Vec<adv_device::gpu::NativeFrame>,
}
fn capture(env: &mut Env<'_>) -> Outcome {
    let pointer = env.ext.get::<Evidence>().device;
    let state = recording::draw_key(env, pointer);
    let keys = recording::command_keys(env, pointer);
    let cpu = env.cpu.capture_architecture().unwrap();
    let mut memory = Vec::new();
    for &(address, len) in &env.ext.get::<Evidence>().ranges {
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
        let frame = gpu.present(&image, serial)?;
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
    assert_eq!(evidence.snapshots[0].2, evidence.snapshots[1].2);
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
    let images: Vec<_> = evidence
        .frames
        .iter()
        .map(|frame| frame.image.clone())
        .collect();
    assert_eq!(images.len(), 2);
    assert_eq!(f.pixels(&images[0]), f.pixels(&images[1]));
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
