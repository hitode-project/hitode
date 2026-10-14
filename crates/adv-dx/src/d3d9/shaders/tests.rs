use super::super::device as current_device;
use super::*;
use adv_cpu::backend::Guest;
use adv_cpu::backend::Reg;

const VS: &[u32] = &[
    0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000, 0xffff,
];
const PS: &[u32] = &[0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff];
fn machine() -> Machine<adv_cpu::JitCpu> {
    let mut m = super::super::tests::machine();
    m.sys.objects = Objects::default();
    m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
    m
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
fn words(env: &mut Env<'_>, tokens: &[u32]) -> GuestPtr<u32> {
    let at = alloc(env, tokens.len() as u32 * 4).unwrap();
    let data: Vec<_> = tokens.iter().flat_map(|v| v.to_le_bytes()).collect();
    env.cpu.write(at, &data).unwrap();
    GuestPtr::new(at)
}
fn shader_object(env: &mut Env<'_>, device: u32, tokens: &[u32], stage: Stage) -> u32 {
    let input = words(env, tokens);
    let output = GuestPtr::new(alloc(env, 4).unwrap());
    assert_eq!(
        create_shader(env, GuestPtr::new(device), input, output, stage),
        0
    );
    output.read(env.cpu).unwrap()
}
fn typed_call(env: &mut Env<'_>, registry: &Registry, name: &str, args: &[u32]) -> u32 {
    let stack = 0x5a0000;
    env.cpu.set_reg(Reg::Esp, stack);
    env.cpu.write_u32(stack, 0x12345678).unwrap();
    for (i, &arg) in args.iter().enumerate() {
        env.cpu.write_u32(stack + 4 + i as u32 * 4, arg).unwrap();
    }
    match (registry.get("d3d9.dll", name).unwrap().handler)(env) {
        Outcome::Return(value) => value,
        _ => panic!("unexpected async call {name}"),
    }
}
#[test]
// spec: GFX-SHADER-3
fn bound_shader_can_retain_zero_public_object_and_get_function_is_bounded() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let pointer = shader_object(&mut env, device, VS, Stage::Vertex);
    let shader_id = com(&env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    assert_eq!(env.ext.get::<State>().shader_stats.vertex[4], 1);
    assert_eq!(
        set_vertex(&mut env, GuestPtr::new(device), GuestPtr::new(pointer)),
        0
    );
    assert_eq!(release(&mut env, pointer), 0);
    assert!(bound(&env, shader_id).is_some());
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    let output = GuestPtr::<u32>::new(alloc(&mut env, 64).unwrap());
    assert_eq!(get_vertex(&mut env, GuestPtr::new(device), output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), pointer);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(2));
    let size = GuestPtr::new(alloc(&mut env, 4).unwrap());
    assert_eq!(
        vertex_function(&mut env, GuestPtr::new(pointer), GuestPtr::new(0), size),
        0
    );
    assert_eq!(size.read(env.cpu).unwrap(), VS.len() as u32 * 4);
    env.cpu.write(output.addr(), &[0xa5; 64]).unwrap();
    size.write(env.cpu, 4).unwrap();
    assert_eq!(
        vertex_function(&mut env, GuestPtr::new(pointer), output.cast(), size),
        0
    );
    assert_eq!(size.read(env.cpu).unwrap(), VS.len() as u32 * 4);
    let mut bytes = [0; 64];
    env.cpu.read(output.addr(), &mut bytes).unwrap();
    assert_eq!(&bytes[VS.len() * 4..], &[0xa5; 32]);
    size.write(env.cpu, 64).unwrap();
    assert_eq!(
        vertex_function(&mut env, GuestPtr::new(pointer), output.cast(), size),
        0
    );
    assert_eq!(size.read(env.cpu).unwrap(), VS.len() as u32 * 4);
    let mut copied = [0; 32];
    env.cpu.read(output.addr(), &mut copied).unwrap();
    assert_eq!(
        copied,
        VS.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()[..]
    );
    assert_eq!(release(&mut env, pointer), 0);
    assert_eq!(
        set_vertex(&mut env, GuestPtr::new(device), GuestPtr::new(0)),
        0
    );
    assert!(bound(&env, shader_id).is_none());
}
#[test]
fn token_reader_skips_end_valued_comments_and_immediates_and_rejects_invalid_memory() {
    let mut m = machine();
    m.cpu.map(0x570000, 4096, Prot::RW).unwrap();
    let mut env = env(&mut m);
    let (device, _, _, _) = super::super::tests::setup(&mut env);
    let tokens = [
        0xfffe0200, 0x0001fffe, 0xffff, 0x05000051, 0xa00f0000, 0xffff, 0, 0, 0, 0x02000001,
        0xc00f0000, 0xa0e40000, 0xffff,
    ];
    let pointer = shader_object(&mut env, device, &tokens, Stage::Vertex);
    assert_eq!(shader(&env, pointer).unwrap().parsed.bytecode(), tokens);
    assert_eq!(release(&mut env, pointer), 0);
    let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
    let initial = env.sys.objects.object_count();
    for at in [0, 0xffff_fffc, 0x570ffc] {
        if at == 0x570ffc {
            env.cpu.write_u32(at, 0xfffe0200).unwrap();
        }
        assert_eq!(
            create_vertex(&mut env, GuestPtr::new(device), GuestPtr::new(at), output),
            INVALID
        );
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(env.sys.objects.object_count(), initial);
    }
    let wrong_stage = words(&mut env, PS);
    assert_eq!(
        create_vertex(&mut env, GuestPtr::new(device), wrong_stage, output),
        INVALID
    );
    let malformed = words(&mut env, &[0xfffe0200, 0x01000001, 0xc00f0000, 0xffff]);
    assert_eq!(
        create_vertex(&mut env, GuestPtr::new(device), malformed, output),
        INVALID
    );
    assert_eq!(env.sys.objects.object_count(), initial);
}
#[test]
// spec: GFX-SHADER-3
fn shader_binding_rejects_wrong_stage_foreign_device_and_faulted_outputs() {
    let mut m = machine();
    m.cpu.map(0x580000, 4096, Prot::READ).unwrap();
    let mut env = env(&mut m);
    let (a, id, _, _) = super::super::tests::setup(&mut env);
    let (b, _, _, _) = super::super::tests::setup(&mut env);
    let vs = shader_object(&mut env, a, VS, Stage::Vertex);
    let ps = shader_object(&mut env, a, PS, Stage::Pixel);
    assert_eq!(
        set_vertex(&mut env, GuestPtr::new(a), GuestPtr::new(ps)),
        INVALID
    );
    assert_eq!(
        set_vertex(&mut env, GuestPtr::new(b), GuestPtr::new(vs)),
        INVALID
    );
    assert_eq!(
        set_pixel(&mut env, GuestPtr::new(a), GuestPtr::new(vs)),
        INVALID
    );
    assert_eq!(set_vertex(&mut env, GuestPtr::new(a), GuestPtr::new(vs)), 0);
    let refs = com(&env).references(&env.sys.objects, id);
    assert_eq!(
        get_vertex(&mut env, GuestPtr::new(a), GuestPtr::new(0x580000)),
        INVALID
    );
    assert_eq!(com(&env).references(&env.sys.objects, id), refs);
    assert_eq!(
        pixel_function(
            &mut env,
            GuestPtr::new(vs),
            GuestPtr::new(0),
            GuestPtr::new(0)
        ),
        INVALID
    );
    assert_eq!(set_vertex(&mut env, GuestPtr::new(a), GuestPtr::new(0)), 0);
    assert_eq!(release(&mut env, vs), 0);
    assert_eq!(release(&mut env, ps), 0);
}
#[test]
// spec: GFX-SHADER-3
fn constants_preserve_bits_check_complete_ranges_and_preserve_bool_words() {
    let mut m = machine();
    m.cpu.map(0x590000, 8192, Prot::RW).unwrap();
    m.cpu.protect(0x591000, 4096, Prot::READ).unwrap();
    let mut env = env(&mut m);
    let (device, _, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let input = GuestPtr::<u8>::new(alloc(&mut env, 64).unwrap());
    let output = GuestPtr::<u8>::new(alloc(&mut env, 64).unwrap());
    let values = [f32::from_bits(0x7fc00123), -0.0, f32::INFINITY, -12.5];
    GuestPtr::<[f32; 4]>::new(input.addr())
        .write(env.cpu, values)
        .unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 255, input.cast(), 1), 0);
    assert_eq!(get_vertex_f(&mut env, this, 255, output.cast(), 1), 0);
    let result = GuestPtr::<[f32; 4]>::new(output.addr())
        .read(env.cpu)
        .unwrap();
    assert_eq!(result.map(f32::to_bits), values.map(f32::to_bits));
    for (start, count) in [(255, 2), (256, 1), (u32::MAX, 2), (1, u32::MAX)] {
        assert_eq!(
            set_vertex_f(&mut env, this, start, input.cast(), count),
            INVALID
        );
        assert_eq!(
            get_vertex_f(&mut env, this, start, output.cast(), count),
            INVALID
        );
    }
    assert_eq!(set_pixel_f(&mut env, this, 224, input.cast(), 1), INVALID);
    assert_eq!(set_vertex_f(&mut env, this, 256, GuestPtr::new(0), 0), 0);
    assert_eq!(get_vertex_f(&mut env, this, 256, GuestPtr::new(0), 0), 0);
    assert_eq!(
        set_vertex_f(&mut env, this, 0, GuestPtr::new(0), 1),
        INVALID
    );
    assert_eq!(
        get_vertex_f(&mut env, this, 254, GuestPtr::new(0x590ff8), 1),
        INVALID
    );
    let mut zero = [0; 8];
    env.cpu.read(0x590ff8, &mut zero).unwrap();
    assert_eq!(zero, [0; 8]);
    GuestPtr::<[i32; 4]>::new(input.addr())
        .write(env.cpu, [i32::MIN, i32::MAX, -1, 42])
        .unwrap();
    assert_eq!(set_pixel_i(&mut env, this, 15, input.cast(), 1), 0);
    assert_eq!(get_pixel_i(&mut env, this, 15, output.cast(), 1), 0);
    assert_eq!(
        GuestPtr::<[i32; 4]>::new(output.addr())
            .read(env.cpu)
            .unwrap(),
        [i32::MIN, i32::MAX, -1, 42]
    );
    GuestPtr::<[i32; 4]>::new(input.addr())
        .write(env.cpu, [0, 2, -1, i32::MIN])
        .unwrap();
    assert_eq!(set_vertex_b(&mut env, this, 12, input.cast(), 4), 0);
    assert_eq!(get_vertex_b(&mut env, this, 12, output.cast(), 4), 0);
    assert_eq!(
        GuestPtr::<[i32; 4]>::new(output.addr())
            .read(env.cpu)
            .unwrap(),
        [0, 2, -1, i32::MIN]
    );
    assert_eq!(set_vertex_i(&mut env, this, 15, input.cast(), 2), INVALID);
    assert_eq!(set_pixel_b(&mut env, this, 16, input.cast(), 1), INVALID);
}
#[test]
// spec: GFX-SHADER-4
fn custom_stateblock_records_without_mutating_device_and_capture_keeps_subset() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let pointer = shader_object(&mut env, device, VS, Stage::Vertex);
    let shader_id = com(&env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let input = GuestPtr::<u8>::new(alloc(&mut env, 32).unwrap());
    let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
    GuestPtr::<[f32; 4]>::new(input.addr())
        .write(env.cpu, [1., 2., 3., 4.])
        .unwrap();
    assert_eq!(stateblocks::begin(&mut env, this), 0);
    assert_eq!(stateblocks::begin(&mut env, this), INVALID);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(pointer)), 0);
    assert_eq!(set_vertex_f(&mut env, this, 7, input.cast(), 1), 0);
    assert_eq!(fixed::set_fvf(&mut env, this, 0x42), 0);
    assert_eq!(current_device(&env, device).unwrap().shaders.vertex, None);
    assert_eq!(
        current_device(&env, device)
            .unwrap()
            .shaders
            .vertex_constants
            .floats[7],
        [0.; 4]
    );
    assert_eq!(current_device(&env, device).unwrap().fixed.fvf, 0);
    assert_eq!(stateblocks::end(&mut env, this, output), 0);
    let block = output.read(env.cpu).unwrap();
    assert_eq!(release(&mut env, pointer), 0);
    assert!(bound(&env, shader_id).is_some());
    GuestPtr::<[f32; 4]>::new(input.addr())
        .write(env.cpu, [9.; 4])
        .unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 8, input.cast(), 1), 0);
    assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
    let d = current_device(&env, device).unwrap();
    assert_eq!(d.shaders.vertex, Some(shader_id));
    assert_eq!(d.shaders.vertex_constants.floats[7], [1., 2., 3., 4.]);
    assert_eq!(d.shaders.vertex_constants.floats[8], [9.; 4]);
    assert_eq!(d.fixed.fvf, 0x42);
    assert_eq!(set_vertex_f(&mut env, this, 7, input.cast(), 1), 0);
    assert_eq!(stateblocks::capture(&mut env, GuestPtr::new(block)), 0);
    GuestPtr::<[f32; 4]>::new(input.addr())
        .write(env.cpu, [5.; 4])
        .unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 7, input.cast(), 1), 0);
    assert_eq!(set_vertex_f(&mut env, this, 8, input.cast(), 1), 0);
    assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
    assert_eq!(
        current_device(&env, device)
            .unwrap()
            .shaders
            .vertex_constants
            .floats[7],
        [9.; 4]
    );
    assert_eq!(
        current_device(&env, device)
            .unwrap()
            .shaders
            .vertex_constants
            .floats[8],
        [5.; 4]
    );
    assert_eq!(release(&mut env, block), 0);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(0)), 0);
    assert!(bound(&env, shader_id).is_none());
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
}
#[test]
// spec: GFX-SHADER-4
fn reset_clears_shader_bindings_constants_and_recording_but_keeps_public_program() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let pointer = shader_object(&mut env, device, VS, Stage::Vertex);
    let shader_id = com(&env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(pointer)), 0);
    let input = GuestPtr::<u8>::new(alloc(&mut env, 16).unwrap());
    GuestPtr::<[f32; 4]>::new(input.addr())
        .write(env.cpu, [3.; 4])
        .unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 1, input.cast(), 1), 0);
    assert_eq!(stateblocks::begin(&mut env, this), 0);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(pointer)), 0);
    reset_bindings(&mut env, id);
    assert!(bound(&env, shader_id).is_some());
    let d = current_device(&env, device).unwrap();
    assert!(d.recording.is_none());
    assert!(d.shaders.vertex.is_none());
    assert_eq!(d.shaders.vertex_constants.floats[1], [0.; 4]);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(pointer)), 0);
    assert_eq!(release(&mut env, pointer), 0);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(0)), 0);
    assert!(bound(&env, shader_id).is_none());
}
#[test]
// spec: GFX-SHADER-4
fn native_vertex_and_pixel_block_membership_restores_only_each_stage() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let input = GuestPtr::<[f32; 4]>::new(alloc(&mut env, 16).unwrap());
    let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
    // Independent native probe: SHADEMODE is captured by both block types;
    // SPECULARENABLE, LIGHTING, CULLMODE and material sources are vertex state.
    for (kind, expected) in [(2, [2, 1, 0, 1, 0, 2]), (3, [2, 0, 1, 3, 1, 1])] {
        let states = [9, 29, 137, 22, 27, 145];
        let before = [2, 0, 1, 3, 0, 1];
        let after = [1, 1, 0, 1, 1, 2];
        if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
            for (&state, &value) in states.iter().zip(&before) {
                d.fixed.render.insert(state, value);
            }
        }
        input.write(env.cpu, [1.; 4]).unwrap();
        assert_eq!(set_vertex_f(&mut env, this, 7, input.cast(), 1), 0);
        input.write(env.cpu, [2.; 4]).unwrap();
        assert_eq!(set_pixel_f(&mut env, this, 7, input.cast(), 1), 0);
        assert_eq!(stateblocks::create(&mut env, this, kind, output), 0);
        let block = output.read(env.cpu).unwrap();
        if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
            for (&state, &value) in states.iter().zip(&after) {
                d.fixed.render.insert(state, value);
            }
        }
        input.write(env.cpu, [3.; 4]).unwrap();
        assert_eq!(set_vertex_f(&mut env, this, 7, input.cast(), 1), 0);
        input.write(env.cpu, [4.; 4]).unwrap();
        assert_eq!(set_pixel_f(&mut env, this, 7, input.cast(), 1), 0);
        assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
        let d = current_device(&env, device).unwrap();
        assert_eq!(states.map(|state| d.fixed.render[&state]), expected);
        assert_eq!(
            d.shaders.vertex_constants.floats[7],
            if kind == 3 { [1.; 4] } else { [3.; 4] }
        );
        assert_eq!(
            d.shaders.pixel_constants.floats[7],
            if kind == 2 { [2.; 4] } else { [4.; 4] }
        );
        assert_eq!(release(&mut env, block), 0);
    }
}
#[test]
// spec: GFX-SAMPLER-1
fn typed_recording_keeps_buffers_alive_and_restores_fixed_sampler_and_bump_state() {
    let mut m = machine();
    m.cpu.map(0x5a0000, 4096, Prot::RW).unwrap();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let output = GuestPtr::new(alloc(&mut env, 16).unwrap());
    for sampler in (0..=15).chain(257..=260) {
        assert_eq!(
            typed_call(
                &mut env,
                &registry,
                "IDirect3DDevice9::GetSamplerState",
                &[device, sampler, 3, output.addr()]
            ),
            0
        );
        assert_eq!(output.read(env.cpu).unwrap(), 1);
    }
    assert_eq!(
        typed_call(
            &mut env,
            &registry,
            "IDirect3DDevice9::CreateVertexBuffer",
            &[device, 64, 0, 0, 1, output.addr(), 0]
        ),
        0
    );
    let buffer = output.read(env.cpu).unwrap();
    let buffer_id = com(&env)
        .identity::<Payload>(&env.sys.objects, buffer)
        .unwrap();
    assert_eq!(stateblocks::begin(&mut env, GuestPtr::new(device)), 0);
    for (name, args) in [
        ("IDirect3DDevice9::SetRenderState", vec![device, 137, 0]),
        ("IDirect3DDevice9::SetSamplerState", vec![device, 2, 9, 5]),
        ("IDirect3DDevice9::SetSamplerState", vec![device, 0, 1, 4]),
        ("IDirect3DDevice9::SetSamplerState", vec![device, 0, 3, 3]),
        ("IDirect3DDevice9::SetSamplerState", vec![device, 2, 3, 2]),
        (
            "IDirect3DDevice9::SetSamplerState",
            vec![device, 0, 4, 0x8010_2040],
        ),
        (
            "IDirect3DDevice9::SetSamplerState",
            vec![device, 2, 4, 0xffab_cdef],
        ),
        (
            "IDirect3DDevice9::SetSamplerState",
            vec![device, 257, 4, 0xff12_3456],
        ),
        (
            "IDirect3DDevice9::SetTextureStageState",
            vec![device, 0, 7, 1f32.to_bits()],
        ),
        (
            "IDirect3DDevice9::SetStreamSource",
            vec![device, 1, buffer, 4, 12],
        ),
    ] {
        assert_eq!(typed_call(&mut env, &registry, name, &args), 0);
    }
    assert_eq!(current_device(&env, device).unwrap().fixed.render[&137], 1);
    assert_eq!(current_device(&env, device).unwrap().fixed.sampler[&1], 1);
    assert_eq!(current_device(&env, device).unwrap().fixed.sampler[&3], 1);
    assert_eq!(current_device(&env, device).unwrap().fixed.sampler[&4], 0);
    assert_eq!(
        current_device(&env, device).unwrap().fixed.sampler_slots[2][&9],
        0
    );
    assert_eq!(
        current_device(&env, device).unwrap().fixed.stage_slots[0][&7],
        0
    );
    assert!(
        current_device(&env, device).unwrap().buffers.streams[1]
            .buffer
            .is_none()
    );
    assert_eq!(stateblocks::end(&mut env, GuestPtr::new(device), output), 0);
    let block = output.read(env.cpu).unwrap();
    assert_eq!(release(&mut env, buffer), 0);
    assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
    let d = current_device(&env, device).unwrap();
    assert_eq!(d.fixed.render[&137], 0);
    assert_eq!(d.fixed.sampler_slots[2][&9], 5);
    assert_eq!(d.fixed.sampler[&1], 4);
    assert_eq!(d.fixed.sampler[&4], 0x8010_2040);
    assert_eq!(d.fixed.sampler[&3], 3);
    assert_eq!(d.fixed.sampler_slots[2][&3], 2);
    assert_eq!(d.fixed.sampler_slots[2][&4], 0xffab_cdef);
    assert_eq!(d.fixed.sampler_slots[16][&4], 0xff12_3456);
    assert_eq!(
        d.fixed.program_sampler(0).unwrap().border_color,
        0x8040_2010
    );
    assert_eq!(d.fixed.stage_slots[0][&7], 1f32.to_bits());
    assert_eq!(d.buffers.streams[1].buffer, Some(buffer_id));
    assert_eq!(
        (d.buffers.streams[1].offset, d.buffers.streams[1].stride),
        (4, 12)
    );
    assert_eq!(release(&mut env, block), 0);
    buffers::reset_bindings(&mut env, id);
    assert!(
        com(&env)
            .get_identity(&env.sys.objects, buffer_id)
            .is_none()
    );
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
}
#[test]
// spec: GFX-SAMPLER-2
fn neutral_sampler_defaults_pair_queries_changes_and_state_blocks_for_every_slot() {
    let mut m = machine();
    m.cpu.map(0x5a0000, 4096, Prot::RW).unwrap();
    let mut env = env(&mut m);
    let (device, _, _, _) = super::super::tests::setup(&mut env);
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    let output = GuestPtr::<u32>::new(alloc(&mut env, 16).unwrap());
    for sampler in (0..=15).chain(257..=260) {
        for (kind, value) in [(8, 0), (10, 1), (11, 0), (12, 0), (13, 0)] {
            assert_eq!(
                typed_call(
                    &mut env,
                    &registry,
                    "IDirect3DDevice9::GetSamplerState",
                    &[device, sampler, kind, output.addr()]
                ),
                0
            );
            assert_eq!(output.read(env.cpu).unwrap(), value);
            assert_eq!(
                typed_call(
                    &mut env,
                    &registry,
                    "IDirect3DDevice9::SetSamplerState",
                    &[device, sampler, kind, value]
                ),
                0
            );
        }
        assert_eq!(stateblocks::begin(&mut env, GuestPtr::new(device)), 0);
        assert_eq!(
            typed_call(
                &mut env,
                &registry,
                "IDirect3DDevice9::SetSamplerState",
                &[device, sampler, 8, 0x80000000]
            ),
            0
        );
        assert_eq!(stateblocks::end(&mut env, GuestPtr::new(device), output), 0);
        let block = output.read(env.cpu).unwrap();
        assert_eq!(
            typed_call(
                &mut env,
                &registry,
                "IDirect3DDevice9::GetSamplerState",
                &[device, sampler, 8, output.addr()]
            ),
            0
        );
        assert_eq!(
            output.read(env.cpu).unwrap(),
            0,
            "recording leaves current defaults intact"
        );
        assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
        assert_eq!(
            typed_call(
                &mut env,
                &registry,
                "IDirect3DDevice9::GetSamplerState",
                &[device, sampler, 8, output.addr()]
            ),
            0
        );
        assert_eq!(
            output.read(env.cpu).unwrap(),
            0x80000000,
            "signed zero survives state blocks"
        );
        for (kind, value, previous) in [
            (8, 0x3f800000, 0x80000000),
            (8, 0x7f800000, 0x80000000),
            (10, 2, 1),
            (11, 1, 0),
            (12, 1, 0),
            (13, 1, 0),
        ] {
            assert_eq!(
                typed_call(
                    &mut env,
                    &registry,
                    "IDirect3DDevice9::SetSamplerState",
                    &[device, sampler, kind, value]
                ),
                UNAVAILABLE
            );
            assert_eq!(
                typed_call(
                    &mut env,
                    &registry,
                    "IDirect3DDevice9::GetSamplerState",
                    &[device, sampler, kind, output.addr()]
                ),
                0
            );
            assert_eq!(output.read(env.cpu).unwrap(), previous);
        }
        assert_eq!(release(&mut env, block), 0);
    }
}

#[test]
fn failed_block_retain_rolls_back_partial_ownership_and_unchanged_capture_reuses_refs() {
    let mut m = super::super::tests::machine();
    let mut env = env(&mut m);
    let (device, id, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let vertex = shader_object(&mut env, device, VS, Stage::Vertex);
    let vertex_id = com(&env)
        .identity::<Payload>(&env.sys.objects, vertex)
        .unwrap();
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(vertex)), 0);
    assert_eq!(release(&mut env, vertex), 0);
    let pixel = shader_object(&mut env, device, PS, Stage::Pixel);
    let pixel_id = com(&env)
        .identity::<Payload>(&env.sys.objects, pixel)
        .unwrap();
    assert_eq!(set_pixel(&mut env, this, GuestPtr::new(pixel)), 0);
    let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
    let objects = env.sys.objects.object_count();
    let vertex_refs = env.sys.objects.strong_count(vertex_id);
    let pixel_refs = env.sys.objects.strong_count(pixel_id);
    assert_eq!(
        stateblocks::create(&mut env, this, 1, output),
        OUT_OF_MEMORY
    );
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(env.sys.objects.object_count(), objects);
    assert_eq!(env.sys.objects.strong_count(vertex_id), vertex_refs);
    assert_eq!(env.sys.objects.strong_count(pixel_id), pixel_refs);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(2));
    assert_eq!(release(&mut env, pixel), 0);
    assert_eq!(stateblocks::create(&mut env, this, 1, output), 0);
    let block = output.read(env.cpu).unwrap();
    // The identity's artificial reference limit is now reached. Capture/Apply
    // of an unchanged binding must transfer existing ownership without a retain.
    assert_eq!(stateblocks::capture(&mut env, GuestPtr::new(block)), 0);
    assert_eq!(stateblocks::apply(&mut env, GuestPtr::new(block)), 0);
    assert_eq!(release(&mut env, block), 0);
    assert_eq!(set_vertex(&mut env, this, GuestPtr::new(0)), 0);
    assert_eq!(set_pixel(&mut env, this, GuestPtr::new(0)), 0);
    assert!(bound(&env, vertex_id).is_none());
    assert!(bound(&env, pixel_id).is_none());
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
}

// spec: GFX-SHADER-3, GFX-CACHE-5
#[test]
fn repeated_programs_share_translation_but_keep_distinct_objects() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, _, _, _) = super::super::tests::setup(&mut env);
    for (tokens, stage) in [(VS, Stage::Vertex), (PS, Stage::Pixel)] {
        let first = shader_object(&mut env, device, tokens, stage);
        let second = shader_object(&mut env, device, tokens, stage);
        assert_ne!(first, second);
        let a = shader(&env, first).unwrap();
        let b = shader(&env, second).unwrap();
        assert_eq!(a.parsed.bytecode(), tokens);
        assert_eq!(b.parsed.bytecode(), tokens);
        assert!(Arc::ptr_eq(
            a.translation.as_ref().unwrap(),
            b.translation.as_ref().unwrap()
        ));
        assert_eq!(release(&mut env, first), 0);
        assert!(shader(&env, second).is_some());
        assert_eq!(release(&mut env, second), 0);
    }
    assert_eq!(env.ext.get::<State>().shader_translations.entries.len(), 2);
}

// spec: GFX-CACHE-5
#[test]
fn translation_cache_evicts_without_changing_retained_programs() {
    let mut cache = TranslationCache::default();
    let first = crate::shader::Shader::parse(PS).unwrap();
    let retained = cache.translate(&first, None).unwrap();
    for index in 0..256 {
        let mut tokens = PS[..PS.len() - 1].to_vec();
        // Comments change bytecode identity without changing the authored shader.
        tokens.extend_from_slice(&[0x0001fffe, index, 0xffff]);
        let parsed = crate::shader::Shader::parse(&tokens).unwrap();
        assert_eq!(
            cache.translate(&parsed, None).unwrap().source,
            retained.source
        );
    }
    assert_eq!(cache.entries.len(), 1);
    let replacement = cache.translate(&first, None).unwrap();
    assert!(!Arc::ptr_eq(&retained, &replacement));
    assert_eq!(retained.source, replacement.source);
}

// spec: GFX-CACHE-5
#[test]
fn translation_cache_bounds_large_bytecode_payloads() {
    let mut cache = TranslationCache::default();
    for index in 0..40 {
        let mut tokens = PS[..PS.len() - 1].to_vec();
        for _ in 0..2 {
            tokens.push((32_000 << 16) | 0xfffe);
            tokens.extend(std::iter::repeat_n(index, 32_000));
        }
        tokens.push(0xffff);
        let shader = crate::shader::Shader::parse(&tokens).unwrap();
        cache.translate(&shader, None).unwrap();
        assert!(cache.bytes <= 8 * 1024 * 1024);
    }
    assert!(
        cache.entries.len() < 40,
        "byte budget must evict before the entry limit"
    );
}

#[test]
// spec: GFX-SHADER-4
fn repeated_capture_preserves_signed_zero_nan_and_uncaptured_constants() {
    let mut m = machine();
    let mut env = env(&mut m);
    let (device, _, _, _) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(device);
    let input = words(&mut env, &[0, 0x7fc0_0123, 0, 0]).cast();
    let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
    assert_eq!(stateblocks::begin(&mut env, this), 0);
    assert_eq!(set_vertex_f(&mut env, this, 7, input, 1), 0);
    assert_eq!(stateblocks::end(&mut env, this, output), 0);
    let block = GuestPtr::new(output.read(env.cpu).unwrap());
    assert_eq!(set_vertex_f(&mut env, this, 7, input, 1), 0);
    assert_eq!(stateblocks::capture(&mut env, block), 0);
    assert_eq!(stateblocks::capture(&mut env, block), 0);
    env.cpu.write_u32(input.addr(), 0x8000_0000).unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 7, input, 1), 0);
    assert_eq!(stateblocks::capture(&mut env, block), 0);
    env.cpu.write_u32(input.addr(), 1f32.to_bits()).unwrap();
    assert_eq!(set_vertex_f(&mut env, this, 7, input, 1), 0);
    assert_eq!(set_vertex_f(&mut env, this, 8, input, 1), 0);
    assert_eq!(stateblocks::apply(&mut env, block), 0);
    let c = &current_device(&env, device)
        .unwrap()
        .shaders
        .vertex_constants;
    assert_eq!(
        c.floats[7].map(f32::to_bits),
        [0x8000_0000, 0x7fc0_0123, 0, 0]
    );
    assert_eq!(c.floats[8][0], 1.);
}
