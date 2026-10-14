//! Authored padded position/UV streams feed the existing fixed draw and GPU.
use super::*;
use adv_abi::types::D3DSURFACE_DESC;

const STACK: u32 = 0x550000;
#[derive(Default)]
struct Views {
    next: u32,
    targets: BTreeMap<u32, Target>,
}
fn env(m: &mut Machine<adv_cpu::JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn invoke(m: &mut Machine<adv_cpu::JitCpu>, name: &str, args: &[u32]) -> Outcome {
    let mut registry = Registry::default();
    super::super::super::register(&mut registry);
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, STACK);
    m.cpu.write_u32(STACK, 0x12345678).unwrap();
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
    }
    (registry.get("d3d9.dll", name).unwrap().handler)(&mut env(m))
}
fn okay(outcome: Outcome) {
    match outcome {
        Outcome::Return(value) => assert_eq!(value, 0),
        _ => panic!("expected synchronous success"),
    }
}
fn slot(m: &mut Machine<adv_cpu::JitCpu>) -> u32 {
    alloc(&mut env(m), 4).unwrap()
}
fn vertex(
    m: &mut Machine<adv_cpu::JitCpu>,
    device: u32,
    len: u32,
    offset: u32,
    bytes: &[u8],
) -> u32 {
    let out = slot(m);
    okay(invoke(
        m,
        "IDirect3DDevice9::CreateVertexBuffer",
        &[device, len, 0, 0, 1, out, 0],
    ));
    let pointer = m.cpu.read_u32(out).unwrap();
    write_vertex(m, pointer, offset, bytes);
    pointer
}
fn write_vertex(m: &mut Machine<adv_cpu::JitCpu>, pointer: u32, offset: u32, bytes: &[u8]) {
    let out = slot(m);
    okay(invoke(
        m,
        "IDirect3DVertexBuffer9::Lock",
        &[pointer, offset, bytes.len() as u32, out, 0],
    ));
    m.cpu.write(m.cpu.read_u32(out).unwrap(), bytes).unwrap();
    okay(invoke(m, "IDirect3DVertexBuffer9::Unlock", &[pointer]));
}
fn pixels(m: &mut Machine<adv_cpu::JitCpu>, target: &Target) -> Vec<u8> {
    let mut transfer = m
        .ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .readback(&target.image, None)
        .unwrap();
    transfer.take_completion_job().unwrap()();
    transfer.poll().unwrap().unwrap()
}
fn clear_frame(m: &mut Machine<adv_cpu::JitCpu>, device: u32) {
    okay(invoke(
        m,
        "IDirect3DDevice9::Clear",
        &[device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
    ));
}
fn draws(m: &Machine<adv_cpu::JitCpu>) -> u64 {
    m.ext.get::<State>().gpu.as_ref().unwrap().stats().draws
}

#[test]
fn cpu_multistream_fixed_input_preserves_offsets_indices_uv_pixels_and_rejection_atomicity() {
    let mut m = super::super::super::tests::machine();
    m.sys.objects = Objects::default();
    m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
    m.spawn("multistream fixture", 0x400000, &[], 65536)
        .unwrap();
    m.ext.insert(Views::default());
    let gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let target = gpu.target(16, 12, false).unwrap();
    let state = m.ext.get_mut::<State>();
    state.gpu = Some(gpu);
    state.tables.insert("IDirect3DTexture9", 0);
    state.tables.insert("IDirect3DVertexDeclaration9", 0);
    state.provider.insert = |ext, target| {
        let views = ext.get_mut::<Views>();
        views.next += 1;
        views.targets.insert(views.next, target);
        Ok(views.next)
    };
    state.provider.target = |ext, id| ext.get::<Views>().targets.get(&id).cloned();
    state.provider.remove = |ext, _, id| {
        ext.get_mut::<Views>()
            .targets
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| "missing fixture view".into())
    };
    let frame_surface = {
        let provider = m.ext.get::<State>().provider;
        (provider.insert)(&mut m.ext, target.clone()).unwrap()
    };
    let (device, device_id, _, back) = super::super::super::tests::setup(&mut env(&mut m));
    if let Some(Payload::Resource(r)) = com(&env(&mut m)).get_identity_mut(&mut m.sys.objects, back)
    {
        r.desc = D3DSURFACE_DESC {
            Format: 21,
            Type: 1,
            Pool: 0,
            Width: 16,
            Height: 12,
            ..D3DSURFACE_DESC::default()
        };
        r.surface = Some(frame_surface);
    }
    if let Some(Payload::Device(d)) =
        com(&env(&mut m)).get_identity_mut(&mut m.sys.objects, device_id)
    {
        d.surface = frame_surface;
        d.back_buffer = Some(back);
    }
    for (state, value) in [(137, 0), (7, 0), (22, 1)] {
        okay(invoke(
            &mut m,
            "IDirect3DDevice9::SetRenderState",
            &[device, state, value],
        ));
    }
    for state in [5, 6] {
        okay(invoke(
            &mut m,
            "IDirect3DDevice9::SetSamplerState",
            &[device, 0, state, 1],
        ));
    }
    okay(invoke(&mut m, "IDirect3DDevice9::BeginScene", &[device]));
    // Different per-stream padding, plus SetStreamSource offsets. Vertex zero
    // contains nonfinite sentinels and must be skipped by StartVertex/BaseIndex.
    let positions = [
        [0., f32::NAN, f32::NAN, f32::NAN],
        [f32::NAN, -1., 1., 0.5],
        [f32::NAN, 1., 1., 0.5],
        [f32::NAN, -1., -1., 0.5],
        [f32::NAN, 1., -1., 0.5],
    ];
    let mut uv = [[f32::NAN, f32::NAN, f32::NAN]; 5];
    uv[1..].fill([f32::NAN, 0.75, 0.25]);
    let pos = vertex(&mut m, device, 84, 4, bytemuck::cast_slice(&positions));
    let tex = vertex(&mut m, device, 8192, 4100, bytemuck::cast_slice(&uv));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetStreamSource",
        &[device, 0, pos, 4, 16],
    ));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetStreamSource",
        &[device, 1, tex, 4100, 12],
    ));
    let elements = [
        0, 0, 4, 0, 2, 0, 0, 0, 1, 0, 4, 0, 1, 0, 5, 0, 0xff, 0, 0, 0, 17, 0, 0, 0,
    ];
    let element_pointer = alloc(&mut env(&mut m), 24).unwrap();
    m.cpu.write(element_pointer, &elements).unwrap();
    let output = slot(&mut m);
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::CreateVertexDeclaration",
        &[device, element_pointer, output],
    ));
    let declaration = m.cpu.read_u32(output).unwrap();
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetVertexDeclaration",
        &[device, declaration],
    ));
    let image = [
        vec![0, 0, 255, 255],
        vec![0, 255, 0, 255],
        vec![255, 0, 0, 255],
        vec![255, 255, 255, 255],
    ]
    .concat();
    let Outcome::Return(texture) =
        super::super::super::loading::create(&mut env(&mut m), device, 2, 2, 21, 1, image)
    else {
        panic!("small upload must complete");
    };
    assert!(texture != 0 && texture & 0x80000000 == 0);
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, texture],
    ));
    let prepared = fixed::prepare_bound(&mut env(&mut m), device, 5, 2, 1, None).unwrap();
    assert!(prepared.vertices.iter().all(|v| v.uv == [0.75, 0.25]));
    assert_eq!(
        prepared.vertices[0].position,
        [-1. + 1. / 16., 1. - 1. / 12., 0.5, 1.]
    );
    drop(prepared);
    clear_frame(&mut m, device);
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::DrawPrimitive",
        &[device, 5, 1, 2],
    ));
    let at = (4 * 16 + 6) * 4;
    assert_eq!(&pixels(&mut m, &target)[at..at + 4], &[0, 255, 0, 255]);
    // An indexed batch shares the same stream decoder. Signed base -1 and a
    // skipped sentinel index select physical vertices 1..4 in both streams.
    uv[1..].fill([f32::NAN, 0.25, 0.75]);
    write_vertex(&mut m, tex, 4100, bytemuck::cast_slice(&uv));
    let out = slot(&mut m);
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::CreateIndexBuffer",
        &[device, 14, 0, 101, 1, out, 0],
    ));
    let index = m.cpu.read_u32(out).unwrap();
    okay(invoke(
        &mut m,
        "IDirect3DIndexBuffer9::Lock",
        &[index, 0, 14, out, 0],
    ));
    let index_data = m.cpu.read_u32(out).unwrap();
    m.cpu
        .write(
            index_data,
            bytemuck::cast_slice(&[0xffffu16, 2, 3, 4, 4, 3, 5]),
        )
        .unwrap();
    okay(invoke(&mut m, "IDirect3DIndexBuffer9::Unlock", &[index]));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetIndices",
        &[device, index],
    ));
    clear_frame(&mut m, device);
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::DrawIndexedPrimitive",
        &[device, 4, u32::MAX, 2, 4, 1, 2],
    ));
    assert_eq!(&pixels(&mut m, &target)[at..at + 4], &[255, 0, 0, 255]);
    let before = draws(&m);
    // Missing, too-small, out-of-buffer, locked and unreadable UV streams fail
    // before GPU submission rather than using positions as substitute UVs.
    for (pointer, offset, stride) in [(0, 0, 0), (tex, 4100, 8), (tex, 8176, 12)] {
        okay(invoke(
            &mut m,
            "IDirect3DDevice9::SetStreamSource",
            &[device, 1, pointer, offset, stride],
        ));
        assert!(matches!(
            invoke(
                &mut m,
                "IDirect3DDevice9::DrawPrimitive",
                &[device, 5, 1, 2]
            ),
            Outcome::Return(INVALID)
        ));
    }
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetStreamSource",
        &[device, 1, tex, 4100, 12],
    ));
    let lock = slot(&mut m);
    okay(invoke(
        &mut m,
        "IDirect3DVertexBuffer9::Lock",
        &[tex, 4100, 60, lock, 0],
    ));
    assert!(matches!(
        invoke(
            &mut m,
            "IDirect3DDevice9::DrawPrimitive",
            &[device, 5, 1, 2]
        ),
        Outcome::Return(INVALID)
    ));
    okay(invoke(&mut m, "IDirect3DVertexBuffer9::Unlock", &[tex]));
    let uv_data = buffer(&env(&mut m), tex).unwrap().storage + 4100;
    m.cpu
        .protect(uv_data & !4095, 4096, Prot::default())
        .unwrap();
    assert!(matches!(
        invoke(
            &mut m,
            "IDirect3DDevice9::DrawPrimitive",
            &[device, 5, 1, 2]
        ),
        Outcome::Return(INVALID)
    ));
    m.cpu.protect(uv_data & !4095, 4096, Prot::RW).unwrap();
    assert_eq!(draws(&m), before);
    assert_eq!(&pixels(&mut m, &target)[at..at + 4], &[255, 0, 0, 255]);
    let position_data = buffer(&env(&mut m), pos).unwrap().storage + 4;
    assert!(matches!(
        invoke(
            &mut m,
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[device, 5, 2, position_data, 16]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(_, reason)| reason.contains("all inputs on stream 0"))
    );
    okay(invoke(&mut m, "IDirect3DDevice9::EndScene", &[device]));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetStreamSource",
        &[device, 0, 0, 0, 0],
    ));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetStreamSource",
        &[device, 1, 0, 0, 0],
    ));
    okay(invoke(&mut m, "IDirect3DDevice9::SetIndices", &[device, 0]));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetVertexDeclaration",
        &[device, 0],
    ));
    okay(invoke(
        &mut m,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, 0],
    ));
    for pointer in [pos, tex, index, declaration, texture, device] {
        assert_eq!(release(&mut env(&mut m), pointer), 0);
    }
    assert!(m.ext.get::<Views>().targets.is_empty());
}
