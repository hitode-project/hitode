// Defaults and valid enum values: Microsoft SDK D3DRENDERSTATETYPE,
// D3DVERTEXBLENDFLAGS, D3DTEXTURESTAGESTATETYPE and D3DTEXTURETRANSFORMFLAGS.
mod state_tests {
    use super::*;
    use adv_abi::{Api, ApiDiagnostics, RunMode};
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Reg},
    };
    use adv_machine::{layout::teb_addr, trace::Trace};
    use std::{cell::RefCell, io::Write, rc::Rc};

    const STACK: u32 = 0x550000;
    const DATA: u32 = 0x560000;
    const DEFAULTS: &[(i32, u32)] = &[
        (8, 3),
        (16, 1),
        (28, 0),
        (136, 1),
        (145, 1),
        (146, 2),
        (147, 0),
        (148, 0),
        (151, 0),
        (152, 0),
        (161, 1),
        (167, 0),
    ];

    fn machine() -> Machine<JitCpu> {
        let mut machine = super::super::super::tests::machine();
        machine.cpu.map(STACK, 0x1000, Prot::RW).unwrap();
        machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
        machine.cpu.map(teb_addr(0), 0x1000, Prot::RW).unwrap();
        machine
            .cpu
            .write_u32(teb_addr(0) + 0x34, 0x12345678)
            .unwrap();
        machine.cpu.set_reg(Reg::Esp, STACK);
        machine
    }
    fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
    }
    /// SetVertexShaderConstantF of one register from guest memory.
    fn vertex_constant(env: &mut Env<'_>, pointer: u32, start: u32, data: &[u8]) -> u32 {
        env.cpu.write(DATA + 0x1000, data).unwrap();
        call(
            env,
            super::super::shaders::set_vertex_f_ABI,
            &[pointer, start, DATA + 0x1000, 1],
        )
    }
    fn call(env: &mut Env<'_>, api: Api, args: &[u32]) -> u32 {
        env.cpu.write_u32(STACK, 0x12340000).unwrap();
        for (index, word) in args.iter().enumerate() {
            env.cpu
                .write_u32(STACK + 4 + index as u32 * 4, *word)
                .unwrap();
        }
        match (api.handler)(env) {
            Outcome::Return(value) => value,
            _ => panic!("{} should return synchronously", api.name),
        }
    }
    fn render(env: &mut Env<'_>, pointer: u32, state: i32) -> u32 {
        assert_eq!(call(env, get_render_ABI, &[pointer, state as u32, DATA]), 0);
        env.cpu.read_u32(DATA).unwrap()
    }
    fn stage(env: &mut Env<'_>, pointer: u32, state: i32) -> u32 {
        assert_eq!(
            call(env, get_stage_ABI, &[pointer, 0, state as u32, DATA]),
            0
        );
        env.cpu.read_u32(DATA).unwrap()
    }
    fn defaults(env: &mut Env<'_>, pointer: u32) {
        for &(state, value) in DEFAULTS {
            assert_eq!(render(env, pointer, state), value, "default state {state}");
        }
        assert_eq!(stage(env, pointer, 11), 0);
        assert_eq!(stage(env, pointer, 24), 0);
    }

    // spec: GFX-SHADER-4, GFX-REC-7
    #[test]
    fn identical_writes_keep_versions_and_recorded_membership_preserves_float_bits() {
        use super::super::shaders::stateblocks;
        let mut machine = machine();
        let mut env = env(&mut machine);
        let (pointer, _, _, _) = super::super::super::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let initial = device(&env, pointer).unwrap().versions;
        let sampler = device(&env, pointer).unwrap().fixed.sampler[&5];
        for _ in 0..2 {
            assert_eq!(call(&mut env, set_render_ABI, &[pointer, 22, 3]), 0);
            assert_eq!(call(&mut env, set_stage_ABI, &[pointer, 1, 11, 1]), 0);
            assert_eq!(
                call(&mut env, set_sampler_ABI, &[pointer, 0, 5, sampler]),
                0
            );
            assert_eq!(vertex_constant(&mut env, pointer, 3, &[0; 16]), 0);
        }
        assert_eq!(device(&env, pointer).unwrap().versions, initial);
        assert_eq!(stateblocks::begin(&mut env, this), 0);
        assert_eq!(call(&mut env, set_render_ABI, &[pointer, 22, 3]), 0);
        assert_eq!(vertex_constant(&mut env, pointer, 3, &[0; 16]), 0);
        assert_eq!(stateblocks::end(&mut env, this, GuestPtr::new(DATA)), 0);
        let block = GuestPtr::new(env.cpu.read_u32(DATA).unwrap());
        assert_eq!(device(&env, pointer).unwrap().versions, initial);
        let bits = [0x8000_0000u32, 0x7fc0_1234, 0, 0];
        assert_eq!(
            vertex_constant(&mut env, pointer, 3, bytemuck::cast_slice(&bits)),
            0
        );
        let changed = device(&env, pointer).unwrap().versions;
        assert_eq!(changed[13], initial[13] + 1);
        assert_eq!(
            vertex_constant(&mut env, pointer, 3, bytemuck::cast_slice(&bits)),
            0
        );
        assert_eq!(device(&env, pointer).unwrap().versions, changed);
        assert_eq!(call(&mut env, set_render_ABI, &[pointer, 22, 1]), 0);
        assert_eq!(call(&mut env, set_render_ABI, &[pointer, 14, 0]), 0);
        assert_eq!(stateblocks::apply(&mut env, block), 0);
        let d = device(&env, pointer).unwrap();
        assert_eq!(d.fixed.render[&22], 3);
        assert_eq!(d.fixed.render[&14], 0, "uncaptured state is retained");
        assert_eq!(
            d.shaders.vertex_constants.floats[3].map(f32::to_bits),
            [0; 4]
        );
        let restored = d.versions;
        assert_eq!(stateblocks::apply(&mut env, block), 0);
        assert_eq!(device(&env, pointer).unwrap().versions, restored);
    }

    // spec: GFX-REC-2, GFX-SHADER-4
    #[test]
    fn applying_only_constants_preserves_the_fixed_state_key() {
        let mut machine = machine();
        let mut env = env(&mut machine);
        let (pointer, _, _, _) = super::super::super::tests::setup(&mut env);
        assert_eq!(
            super::super::shaders::stateblocks::create(
                &mut env,
                GuestPtr::new(pointer),
                1,
                GuestPtr::new(DATA)
            ),
            0
        );
        let block = env.cpu.read_u32(DATA).unwrap();
        let key = device(&env, pointer).unwrap().fixed.key();
        assert_eq!(vertex_constant(&mut env, pointer, 0, &[0x3f; 16]), 0);
        assert_eq!(
            super::super::shaders::stateblocks::apply(&mut env, GuestPtr::new(block)),
            0
        );
        let d = device(&env, pointer).unwrap();
        assert_eq!(d.fixed.key.get(), Some(key));
        assert_eq!(d.shaders.vertex_constants.floats[0], [0.; 4]);
        assert_eq!(call(&mut env, set_render_ABI, &[pointer, 22, 1]), 0);
        device(&env, pointer).unwrap().fixed.key();
        assert_eq!(
            super::super::shaders::stateblocks::apply(&mut env, GuestPtr::new(block)),
            0
        );
        let d = device(&env, pointer).unwrap();
        assert_eq!(d.fixed.key.get(), None);
        assert_eq!(d.fixed.key(), key);
    }

    #[test]
    fn typed_neutral_defaults_bool_and_material_sources_roundtrip_without_side_effects() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        assert_eq!(set_render_ABI.conv, adv_abi::Conv::Stdcall { argc: 3 });
        assert_eq!(set_stage_ABI.conv, adv_abi::Conv::Stdcall { argc: 4 });
        let mut machine = machine();
        let mut env = env(&mut machine);
        let (pointer, owner, _, _) = super::super::super::tests::setup(&mut env);
        let objects = env.sys.objects.object_count();
        let refs = env.sys.objects.strong_count(owner);
        defaults(&mut env, pointer);
        for &(state, value) in DEFAULTS {
            assert_eq!(
                call(&mut env, set_render_ABI, &[pointer, state as u32, value]),
                0
            );
            assert_eq!(render(&mut env, pointer, state), value);
        }
        for state in [16, 28, 161] {
            for value in [0, 1, 0] {
                assert_eq!(call(&mut env, set_render_ABI, &[pointer, state, value]), 0);
                assert_eq!(render(&mut env, pointer, state as i32), value);
            }
        }
        for value in [0, 1, 0x3f, 0] {
            assert_eq!(call(&mut env, set_render_ABI, &[pointer, 152, value]), 0);
            assert_eq!(render(&mut env, pointer, 152), value);
        }
        for state in 145..=148 {
            for value in 0..=2 {
                assert_eq!(call(&mut env, set_render_ABI, &[pointer, state, value]), 0);
                assert_eq!(render(&mut env, pointer, state as i32), value);
            }
        }
        for state in [11, 24] {
            assert_eq!(call(&mut env, set_stage_ABI, &[pointer, 0, state, 0]), 0);
            assert_eq!(stage(&mut env, pointer, state as i32), 0);
        }
        assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
        assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x12345678);
        assert_eq!(env.sys.objects.object_count(), objects);
        assert_eq!(env.sys.objects.strong_count(owner), refs);
        assert!(env.ext.get::<State>().pending.is_empty());
        assert!(env.ext.get::<State>().transfers.is_empty());
    }

    #[derive(Clone, Default)]
    struct Capture(Rc<RefCell<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn unsupported_and_invalid_values_preserve_state_and_getters_validate_output_atomically() {
        let mut machine = machine();
        let capture = Capture::default();
        machine.sys.trace = Trace::to_writer(Box::new(capture.clone()));
        let mut env = env(&mut machine);
        let (pointer, owner, wrong, _) = super::super::super::tests::setup(&mut env);
        let refs = env.sys.objects.strong_count(owner);
        let objects = env.sys.objects.object_count();
        let mut unavailable = 0;
        for (state, value, expected) in [
            (8, 1, UNAVAILABLE),
            (8, 2, UNAVAILABLE),
            (8, 0, INVALID),
            (8, 4, INVALID),
            (28, 2, INVALID),
            (136, 2, INVALID),
            (151, 1, UNAVAILABLE),
            (151, 2, UNAVAILABLE),
            (151, 3, UNAVAILABLE),
            (151, 255, UNAVAILABLE),
            (151, 256, UNAVAILABLE),
            (151, 4, INVALID),
            (151, 254, INVALID),
            (151, 257, INVALID),
            (152, 0x40, INVALID),
            (152, 0x80, INVALID),
            (152, u32::MAX, INVALID),
            (167, 1, UNAVAILABLE),
            (167, 2, INVALID),
            (16, 2, INVALID),
            (161, 2, INVALID),
            (145, 3, INVALID),
            (146, 3, INVALID),
            (147, 3, INVALID),
            (148, 3, INVALID),
        ] {
            let previous = render(&mut env, pointer, state);
            assert_eq!(
                call(&mut env, set_render_ABI, &[pointer, state as u32, value]),
                expected,
                "state={state}, value={value:#x}"
            );
            unavailable += u64::from(expected == UNAVAILABLE);
            assert_eq!(render(&mut env, pointer, state), previous);
        }
        for (state, value, expected) in [
            (11, 1, UNAVAILABLE),
            (11, 7, UNAVAILABLE),
            (11, 0x10000, UNAVAILABLE),
            (11, 0x40007, UNAVAILABLE),
            (11, 8, INVALID),
            (11, 0x50000, INVALID),
            (11, u32::MAX, INVALID),
            (24, 1, UNAVAILABLE),
            (24, 2, UNAVAILABLE),
            (24, 3, UNAVAILABLE),
            (24, 4, UNAVAILABLE),
            (24, 256, UNAVAILABLE),
            (24, 257, UNAVAILABLE),
            (24, 258, UNAVAILABLE),
            (24, 259, UNAVAILABLE),
            (24, 260, UNAVAILABLE),
            (24, 5, INVALID),
            (24, 255, INVALID),
            (24, 0x200, INVALID),
        ] {
            let previous = stage(&mut env, pointer, state);
            assert_eq!(
                call(&mut env, set_stage_ABI, &[pointer, 0, state as u32, value]),
                expected,
                "stage state={state}, value={value:#x}"
            );
            unavailable += u64::from(expected == UNAVAILABLE);
            assert_eq!(stage(&mut env, pointer, state), previous);
        }
        for _ in 0..101 {
            assert_eq!(
                call(&mut env, set_render_ABI, &[pointer, 8, 1]),
                UNAVAILABLE
            );
            unavailable += 1;
        }
        for invalid in [0, wrong] {
            assert_eq!(call(&mut env, set_render_ABI, &[invalid, 16, 0]), INVALID);
            assert_eq!(call(&mut env, set_stage_ABI, &[invalid, 0, 11, 0]), INVALID);
        }
        let crossing = DATA + 4094;
        env.cpu.write_u32(crossing, 0xaabbccdd).unwrap();
        env.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
        for api_args in [vec![pointer, 16, crossing], vec![pointer, 0, 11, crossing]] {
            let api = if api_args.len() == 3 {
                get_render_ABI
            } else {
                get_stage_ABI
            };
            assert_eq!(call(&mut env, api, &api_args), INVALID);
            assert_eq!(env.cpu.read_u32(crossing).unwrap(), 0xaabbccdd);
        }
        defaults(&mut env, pointer);
        let diagnostics = env.ext.get::<ApiDiagnostics>();
        assert_eq!(diagnostics.warnings.len(), 5);
        assert_eq!(diagnostics.warnings.values().sum::<u64>(), unavailable);
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(diagnostics.acceptance_eligible(RunMode::Strict));
        let trace = String::from_utf8(capture.0.borrow().clone()).unwrap();
        assert_eq!(trace.lines().count(), 5);
        assert!(
            trace
                .lines()
                .all(|line| line.starts_with("# warning\t") && line.contains("HRESULT=0x8876086a"))
        );
        assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x12345678);
        assert_eq!(env.sys.objects.object_count(), objects);
        assert_eq!(env.sys.objects.strong_count(owner), refs);
    }

    #[derive(Default)]
    struct Surfaces {
        next: u32,
        targets: BTreeMap<u32, Target>,
    }
    fn gpu(env: &mut Env<'_>) {
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .expect("test state/reset fixture");
        env.ext.insert(Surfaces::default());
        let state = env.ext.get_mut::<State>();
        state.gpu = Some(gpu);
        state.provider.insert = |ext, target| {
            let surfaces = ext.get_mut::<Surfaces>();
            surfaces.next += 1;
            surfaces.targets.insert(surfaces.next, target);
            Ok(surfaces.next)
        };
        state.provider.target = |ext, key| ext.get::<Surfaces>().targets.get(&key).cloned();
        state.provider.replace = |ext, key, target| {
            *ext.get_mut::<Surfaces>().targets.get_mut(&key).unwrap() = target;
            Ok(())
        };
        state.provider.remove = |ext, _, key| {
            assert!(ext.get_mut::<Surfaces>().targets.remove(&key).is_some());
            Ok(())
        };
    }

    #[test]
    fn real_reset_restores_defaults_and_material_sources_do_not_replace_unlit_vertex_color_or_uv() {
        let mut machine = machine();
        let mut env = env(&mut machine);
        gpu(&mut env);
        let (factory, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
        let parameters = GuestPtr::new(DATA + 0x100);
        parameters
            .write(
                env.cpu,
                D3DPRESENT_PARAMETERS {
                    BackBufferWidth: 16,
                    BackBufferHeight: 12,
                    BackBufferFormat: 22,
                    BackBufferCount: 1,
                    SwapEffect: 1,
                    hDeviceWindow: 0x7788,
                    Windowed: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            call(
                &mut env,
                create_device_ABI,
                &[factory, 0, 1, 0x7788, 0x24, parameters.addr(), DATA]
            ),
            0
        );
        let pointer = env.cpu.read_u32(DATA).unwrap();
        defaults(&mut env, pointer);
        for state in [16, 161] {
            assert_eq!(call(&mut env, set_render_ABI, &[pointer, state, 0]), 0);
        }
        for state in 145..=148 {
            assert_eq!(call(&mut env, set_render_ABI, &[pointer, state, 2]), 0);
        }
        assert_eq!(call(&mut env, set_stage_ABI, &[pointer, 0, 1, 2]), 0);
        assert_eq!(call(&mut env, reset_ABI, &[pointer, parameters.addr()]), 0);
        defaults(&mut env, pointer);
        assert_eq!(stage(&mut env, pointer, 1), 4);
        assert_eq!(call(&mut env, begin_scene_ABI, &[pointer]), 0);
        for (fvf, position) in [(0x144, 4), (0x142, 3)] {
            assert_eq!(call(&mut env, set_fvf_ABI, &[pointer, fvf]), 0);
            assert_eq!(
                call(
                    &mut env,
                    set_render_ABI,
                    &[pointer, 137, u32::from(position == 4)]
                ),
                0
            );
            let words = [(1.5f32, 1.5f32), (12.5, 1.5), (1.5, 8.5)]
                .into_iter()
                .flat_map(|(x, y)| {
                    let mut words = vec![x.to_bits(), y.to_bits(), 0.5f32.to_bits()];
                    if position == 4 {
                        words.push(1.0f32.to_bits());
                    }
                    words.extend([0xff804020, 0.25f32.to_bits(), 0.75f32.to_bits()]);
                    words
                })
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>();
            let vertices = DATA + 0x400;
            env.cpu.write(vertices, &words).unwrap();
            let stride = words.len() as u32 / 3;
            for value in 0..=2 {
                for state in 145..=148 {
                    assert_eq!(call(&mut env, set_render_ABI, &[pointer, state, value]), 0);
                }
                for last_pixel in [0, 1] {
                    assert_eq!(
                        call(&mut env, set_render_ABI, &[pointer, 16, last_pixel]),
                        0
                    );
                    assert_eq!(
                        call(&mut env, set_render_ABI, &[pointer, 161, last_pixel]),
                        0
                    );
                    let draw = prepare(
                        &mut env,
                        pointer,
                        2,
                        1,
                        GuestPtr::new(vertices),
                        stride,
                        None,
                    )
                    .unwrap();
                    assert_eq!(draw.state.last_pixel, last_pixel != 0);
                    for vertex in draw.vertices.iter() {
                        assert_eq!(vertex.color, [128. / 255., 64. / 255., 32. / 255., 1.]);
                        assert_eq!(vertex.uv, [0.25, 0.75]);
                    }
                }
            }
            if position == 3 {
                assert_eq!(call(&mut env, set_render_ABI, &[pointer, 137, 1]), 0);
                assert_eq!(
                    call(&mut env, draw_up_ABI, &[pointer, 4, 1, vertices, stride]),
                    UNAVAILABLE
                );
                assert_eq!(
                    env.ext
                        .get::<ApiDiagnostics>()
                        .warnings
                        .values()
                        .sum::<u64>(),
                    1
                );
            }
        }
        assert_eq!(call(&mut env, end_scene_ABI, &[pointer]), 0);
        assert_eq!(release(&mut env, pointer), 0);
        assert_eq!(release(&mut env, factory), 0);
        assert_eq!(env.sys.objects.object_count(), 0);
        assert!(env.ext.get::<Surfaces>().targets.is_empty());
    }

    #[test]
    fn permuted_declaration_decodes_like_its_fvf_and_reset_unbinds_it() {
        use super::super::super::declaration;
        let mut machine = machine();
        let mut env = env(&mut machine);
        gpu(&mut env);
        let (factory, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
        let parameters = GuestPtr::new(DATA + 0x100);
        let present = D3DPRESENT_PARAMETERS {
            BackBufferWidth: 16,
            BackBufferHeight: 12,
            BackBufferFormat: 22,
            BackBufferCount: 1,
            SwapEffect: 1,
            hDeviceWindow: 0x7788,
            Windowed: 1,
            ..Default::default()
        };
        parameters.write(env.cpu, present).unwrap();
        assert_eq!(
            call(
                &mut env,
                create_device_ABI,
                &[factory, 0, 1, 0x7788, 0x24, parameters.addr(), DATA]
            ),
            0
        );
        let pointer = env.cpu.read_u32(DATA).unwrap();
        let this = GuestPtr::new(pointer);
        // TEXCOORD0 FLOAT2 @0, COLOR0 D3DCOLOR @8, POSITIONT FLOAT4 @12.
        let elements = DATA + 0x200;
        let mut bytes = Vec::new();
        for (offset, kind, usage) in [(0u16, 1u8, 5u8), (8, 4, 10), (12, 3, 9)] {
            bytes.extend([0, 0]);
            bytes.extend(offset.to_le_bytes());
            bytes.extend([kind, 0, usage, 0]);
        }
        bytes.extend([0xff, 0, 0, 0, 17, 0, 0, 0]);
        env.cpu.write(elements, &bytes).unwrap();
        let output = GuestPtr::<u32>::new(DATA + 0x300);
        assert_eq!(
            declaration::create(&mut env, this, GuestPtr::new(elements), output),
            0
        );
        let decl = output.read(env.cpu).unwrap();
        let corners = [(1.5f32, 1.5f32), (12.5, 1.5), (1.5, 8.5)];
        let vertex = |x: f32, y: f32, permuted: bool| {
            let position = [x.to_bits(), y.to_bits(), 0.5f32.to_bits(), 1f32.to_bits()];
            let rest = [0.25f32.to_bits(), 0.75f32.to_bits()];
            let words: Vec<u32> = if permuted {
                rest.into_iter()
                    .chain([0xff804020])
                    .chain(position)
                    .collect()
            } else {
                position
                    .into_iter()
                    .chain([0xff804020])
                    .chain(rest)
                    .collect()
            };
            words
        };
        assert_eq!(call(&mut env, begin_scene_ABI, &[pointer]), 0);
        let mut decoded = Vec::new();
        for permuted in [false, true] {
            let words: Vec<u8> = corners
                .iter()
                .flat_map(|&(x, y)| vertex(x, y, permuted))
                .flat_map(u32::to_le_bytes)
                .collect();
            let vertices = DATA + 0x400;
            env.cpu.write(vertices, &words).unwrap();
            if permuted {
                assert_eq!(declaration::set(&mut env, this, GuestPtr::new(decl)), 0);
            } else {
                assert_eq!(call(&mut env, set_fvf_ABI, &[pointer, 0x144]), 0);
            }
            let draw = prepare(&mut env, pointer, 4, 1, GuestPtr::new(vertices), 28, None).unwrap();
            decoded.push(draw.vertices);
        }
        let fields = |v: &[adv_device::gpu::Vertex]| -> Vec<_> {
            v.iter().map(|v| (v.position, v.color, v.uv)).collect()
        };
        assert_eq!(fields(&decoded[0]), fields(&decoded[1]));
        assert_eq!(decoded[0][0].uv, [0.25, 0.75]);
        assert_eq!(call(&mut env, end_scene_ABI, &[pointer]), 0);
        // Reset restores the default (no declaration) and drops the binding.
        assert_eq!(release(&mut env, decl), 0);
        assert_eq!(call(&mut env, reset_ABI, &[pointer, parameters.addr()]), 0);
        assert_eq!(declaration::get(&mut env, this, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(release(&mut env, pointer), 0);
        assert_eq!(release(&mut env, factory), 0);
        assert_eq!(env.sys.objects.object_count(), 0);
    }
}

mod fixed_completion {
    use super::lighting::*;
    use super::*;
    use crate::d3d9::shaders::stateblocks;

    fn env(m: &mut Machine<adv_cpu::JitCpu>) -> Env<'_> {
        Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        }
    }
    fn fixture() -> (Machine<adv_cpu::JitCpu>, u32, Target) {
        let mut m = crate::d3d9::tests::machine();
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        let target = gpu.target(16, 12, false).unwrap();
        m.ext.insert(StencilTarget(target.clone()));
        let mut e = env(&mut m);
        let (pointer, owner, _, back) = crate::d3d9::tests::setup(&mut e);
        if let Some(Payload::Resource(r)) = com(&e).get_identity_mut(&mut e.sys.objects, back) {
            r.surface = Some(1);
            r.desc.Format = 21;
            r.desc.Pool = 0;
            r.desc.Width = 16;
            r.desc.Height = 12;
        }
        if let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner) {
            d.scene = true;
            d.render_target = Some(back);
        }
        e.ext.get_mut::<State>().provider.target =
            |ext, key| (key == 1).then(|| ext.get::<StencilTarget>().0.clone());
        e.ext.get_mut::<State>().gpu = Some(gpu);
        assert_eq!(
            crate::d3d9::capabilities::caps(&e).unwrap().MaxActiveLights,
            8
        );
        assert_eq!(set_render(&mut e, GuestPtr::new(pointer), 22, 1), 0);
        (m, pointer, target)
    }
    fn store<T: bytemuck::Pod>(e: &mut Env<'_>, value: T) -> GuestPtr<T> {
        let p = GuestPtr::new(alloc(e, std::mem::size_of::<T>() as u32).unwrap());
        p.write(e.cpu, value).unwrap();
        p
    }
    struct TextureTarget(Target);
    fn texture(e: &mut Env<'_>, pointer: u32, bgra: [u8; 4]) {
        let gpu = e.ext.get_mut::<State>().gpu.as_mut().unwrap();
        let target = gpu.target(1, 1, false).unwrap();
        gpu.upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            4,
            &bgra,
        )
        .unwrap();
        e.ext.insert(TextureTarget(target));
        e.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        let owner = com(e).identity::<Payload>(&e.sys.objects, pointer).unwrap();
        let (_, id) = resources::make(
            e,
            Resource {
                pointer: 0,
                device: owner,
                kind: resources::Kind::Texture,
                desc: adv_abi::types::D3DSURFACE_DESC {
                    Pool: 1,
                    Type: 3,
                    Format: 21,
                    Width: 1,
                    Height: 1,
                    ..Default::default()
                },
                depth: None,
                surface: Some(2),
                storage: 0,
                owns_surface: false,
                lockable: false,
                lock: None,
                container: None,
                levels: Vec::new(),
                level: 0,
                lod: 0,
            },
        )
        .unwrap();
        if let Some(Payload::Device(d)) = com(e).get_identity_mut(&mut e.sys.objects, owner) {
            d.texture = Some(id);
            d.touch(12);
        }
        e.ext.get_mut::<State>().provider.target = |ext, key| match key {
            1 => Some(ext.get::<StencilTarget>().0.clone()),
            2 => Some(ext.get::<TextureTarget>().0.clone()),
            _ => None,
        };
    }
    fn light(e: &mut Env<'_>, pointer: u32, kind: u32, diffuse: [f32; 3]) -> GuestPtr<[u32; 26]> {
        let mut value = Light::default().value;
        value[0] = kind;
        value[1..4].copy_from_slice(&diffuse.map(f32::to_bits));
        value[19] = 10f32.to_bits();
        value[21] = 1f32.to_bits();
        let p = store(e, value);
        assert_eq!(set_light(e, GuestPtr::new(pointer), 0xffff_fffe, p), 0);
        assert_eq!(light_enable(e, GuestPtr::new(pointer), 0xffff_fffe, 1), 0);
        p
    }
    fn pixel(
        e: &mut Env<'_>,
        pointer: u32,
        target: &Target,
        fvf: u32,
        words: &[u32],
        kind: i32,
        count: u32,
    ) -> [u8; 4] {
        assert_eq!(set_fvf(e, GuestPtr::new(pointer), fvf), 0);
        let data = alloc(e, std::mem::size_of_val(words) as u32).unwrap();
        e.cpu.write(data, bytemuck::cast_slice(words)).unwrap();
        assert!(matches!(
            draw_up(
                e,
                GuestPtr::new(pointer),
                kind,
                count,
                GuestPtr::new(data),
                Layout::from_fvf(fvf).unwrap().size
            ),
            Outcome::Return(0)
        ));
        crate::d3d9::tests::flush_recordings(e);
        let mut read = e
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&target.image, None)
            .unwrap();
        read.take_completion_job().unwrap()();
        let pixels = read.poll().unwrap().unwrap();
        pixels[(6 * 16 + 8) * 4..(6 * 16 + 8) * 4 + 4]
            .try_into()
            .unwrap()
    }
    fn close(got: [u8; 4], expected: [u8; 4]) {
        assert!(
            got.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
            "{got:?} expected {expected:?}"
        );
    }
    // spec: GFX-FIXED-1
    #[test]
    fn directional_light_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        light(&mut e, p, 3, [0.5, 0.25, 0.125]);
        // N dot L = 1, white diffuse material, no ambient or emissive.
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x12,
                &[0, 0, 0.5f32.to_bits(), 0, 0, (-1f32).to_bits()],
                1,
                1,
            ),
            [32, 64, 128, 255],
        );
    }
    // spec: GFX-FIXED-1
    #[test]
    fn point_attenuation_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let lp = light(&mut e, p, 1, [1.; 3]);
        let mut l = lp.read(e.cpu).unwrap();
        l[15] = (-1.5f32).to_bits();
        l[22] = 0.5f32.to_bits();
        l[23] = 0.25f32.to_bits();
        lp.write(e.cpu, l).unwrap();
        assert_eq!(set_light(&mut e, GuestPtr::new(p), 0xffff_fffe, lp), 0);
        // Distance 2, attenuation 1/(1 + .5*2 + .25*4) = 1/3.
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x12,
                &[0, 0, 0.5f32.to_bits(), 0, 0, (-1f32).to_bits()],
                1,
                1,
            ),
            [85, 85, 85, 255],
        );
    }
    // spec: GFX-FIXED-2
    #[test]
    fn fog_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        for (state, value) in [(137, 0), (28, 1), (34, 0xff0000ff), (140, 3)] {
            assert_eq!(set_render(&mut e, this, state, value), 0);
        }
        for (mode, distance) in [
            (3, 0.5f32),
            (1, std::f32::consts::LN_2),
            (2, std::f32::consts::LN_2.sqrt()),
        ] {
            assert_eq!(set_render(&mut e, this, 140, mode), 0);
            // Linear factor .5; exp(-ln2) and exp(-sqrt(ln2)^2) are .5.
            close(
                pixel(
                    &mut e,
                    p,
                    &target,
                    0x42,
                    &[0, 0, distance.to_bits(), 0xffff0000],
                    1,
                    1,
                ),
                [128, 0, 128, 255],
            );
        }
        assert_eq!(set_render(&mut e, this, 140, 0), 0);
        assert_eq!(set_render(&mut e, this, 35, 3), 0);
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[0, 0, 0.5f32.to_bits(), 0xffff0000],
                1,
                1,
            ),
            [128, 0, 128, 255],
        );
    }
    // spec: GFX-FIXED-2
    #[test]
    fn texture_factor_modulate_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        assert_eq!(set_render(&mut e, this, 137, 0), 0);
        assert_eq!(set_render(&mut e, this, 60, 0xff804020), 0);
        assert_eq!(set_stage(&mut e, this, 0, 2, 3), 0);
        assert_eq!(set_stage(&mut e, this, 0, 3, 2), 0);
        texture(&mut e, p, [128, 255, 128, 255]);
        // TFACTOR (.502,.251,.125) * texture (.502,1,.502).
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[0, 0, 0.5f32.to_bits(), 0xff80ffff],
                1,
                1,
            ),
            [16, 64, 64, 255],
        );
    }
    // spec: GFX-FIXED-1
    #[test]
    fn lights_material_and_stateblocks() {
        let mut m = crate::d3d9::tests::machine();
        let mut e = env(&mut m);
        let (p, _, _, _) = crate::d3d9::tests::setup(&mut e);
        let this = GuestPtr::new(p);
        let out = store(&mut e, [0u32; 26]);
        assert_eq!(get_light(&mut e, this, 12345, out), INVALID);
        assert_eq!(light_enable(&mut e, this, 12345, 1), 0);
        assert_eq!(get_light(&mut e, this, 12345, out), 0);
        assert_eq!(out.read(e.cpu).unwrap(), Light::default().value);
        let mut custom = Light::default().value;
        custom[1] = 0.5f32.to_bits();
        let input = store(&mut e, custom);
        assert_eq!(set_light(&mut e, this, 12345, input), 0);
        assert_eq!(get_light(&mut e, this, 12345, out), 0);
        assert_eq!(out.read(e.cpu).unwrap(), custom);
        let enabled = store(&mut e, 0i32);
        assert_eq!(get_light_enable(&mut e, this, 12345, enabled), 0);
        assert_eq!(enabled.read(e.cpu).unwrap(), 128);
        let material = store(&mut e, [0.25f32; 17]);
        assert_eq!(set_material(&mut e, this, material), 0);
        let saved = material.read(e.cpu).unwrap();
        assert_eq!(stateblocks::begin(&mut e, this), 0);
        assert_eq!(light_enable(&mut e, this, 12345, 0), 0);
        assert_eq!(set_material(&mut e, this, material), 0);
        let block = store(&mut e, 0u32);
        assert_eq!(stateblocks::end(&mut e, this, block), 0);
        assert_eq!(get_light_enable(&mut e, this, 12345, enabled), 0);
        assert_eq!(enabled.read(e.cpu).unwrap(), 128);
        material.write(e.cpu, [0.; 17]).unwrap();
        assert_eq!(set_material(&mut e, this, material), 0);
        let block = GuestPtr::new(block.read(e.cpu).unwrap());
        assert_eq!(stateblocks::apply(&mut e, block), 0);
        assert_eq!(get_light_enable(&mut e, this, 12345, enabled), 0);
        assert_eq!(enabled.read(e.cpu).unwrap(), 0);
        assert_eq!(get_material(&mut e, this, material), 0);
        assert_eq!(material.read(e.cpu).unwrap(), saved);
        for index in 0..8 {
            assert_eq!(light_enable(&mut e, this, index, 1), 0);
        }
        assert!(active(current(&e, p).unwrap()).is_ok());
        assert_eq!(light_enable(&mut e, this, u32::MAX, 1), 0);
        assert_eq!(
            active(current(&e, p).unwrap()).err().unwrap().code,
            UNAVAILABLE
        );
    }
    // spec: GFX-FIXED-1
    #[test]
    fn multiply_transform_order_and_capture() {
        let mut m = crate::d3d9::tests::machine();
        let mut e = env(&mut m);
        let (p, _, _, _) = crate::d3d9::tests::setup(&mut e);
        let this = GuestPtr::new(p);
        let mut translation = current(&e, p).unwrap().transforms[0];
        translation[12] = 3.;
        let input = store(&mut e, translation);
        assert_eq!(set_transform(&mut e, this, 256, input), 0);
        let mut scale = translation;
        scale[12] = 0.;
        scale[0] = 2.;
        input.write(e.cpu, scale).unwrap();
        assert_eq!(stateblocks::begin(&mut e, this), 0);
        assert_eq!(multiply_transform(&mut e, this, 256, input), 0);
        let output = store(&mut e, [0.; 16]);
        assert_eq!(get_transform(&mut e, this, 256, output), 0);
        assert_eq!(output.read(e.cpu).unwrap(), translation);
        let block = store(&mut e, 0u32);
        assert_eq!(stateblocks::end(&mut e, this, block), 0);
        let block = GuestPtr::new(block.read(e.cpu).unwrap());
        assert_eq!(stateblocks::apply(&mut e, block), 0);
        assert_eq!(get_transform(&mut e, this, 256, output), 0);
        let result = output.read(e.cpu).unwrap();
        assert_eq!((result[0], result[12]), (2., 3.));
    }
    // spec: GFX-FIXED-2
    #[test]
    fn flat_shading_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        assert_eq!(set_render(&mut e, this, 9, 1), 0);
        assert_eq!(set_render(&mut e, this, 137, 0), 0);
        // First vertex's red is constant over the triangle.
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[
                    (-1f32).to_bits(),
                    1f32.to_bits(),
                    0.5f32.to_bits(),
                    0xffff0000,
                    (-1f32).to_bits(),
                    (-3f32).to_bits(),
                    0.5f32.to_bits(),
                    0xff00ff00,
                    3f32.to_bits(),
                    1f32.to_bits(),
                    0.5f32.to_bits(),
                    0xff0000ff,
                ],
                4,
                1,
            ),
            [0, 0, 255, 255],
        );
    }
    // spec: GFX-FIXED-1, GFX-FIXED-2
    #[test]
    fn specular_follows_texture_and_preserves_alpha() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        let lp = light(&mut e, p, 3, [1.; 3]);
        let mut l = lp.read(e.cpu).unwrap();
        l[5..8].fill(1f32.to_bits());
        lp.write(e.cpu, l).unwrap();
        assert_eq!(set_light(&mut e, this, 0xffff_fffe, lp), 0);
        let mut material = current(&e, p).unwrap().material;
        material[3] = 0.5;
        material[8..11].copy_from_slice(&[0.5, 0.25, 0.125]);
        material[16] = 2.;
        let material = store(&mut e, material);
        assert_eq!(set_material(&mut e, this, material), 0);
        assert_eq!(set_render(&mut e, this, 29, 1), 0);
        assert_eq!(set_render(&mut e, this, 142, 0), 0);
        texture(&mut e, p, [0, 0, 0, 255]);
        assert_eq!(set_stage(&mut e, this, 0, 4, 4), 0);
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x12,
                &[0, 0, 0.5f32.to_bits(), 0, 0, (-1f32).to_bits()],
                1,
                1,
            ),
            [32, 64, 128, 128],
        );
    }
    // spec: GFX-FIXED-1, GFX-FIXED-2
    #[test]
    fn prelit_specular_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        assert_eq!(set_render(&mut e, this, 29, 1), 0);
        texture(&mut e, p, [0, 0, 0, 255]);
        assert_eq!(set_stage(&mut e, this, 0, 4, 4), 0);
        close(
            pixel(
                &mut e,
                p,
                &target,
                0xc4,
                &[
                    8f32.to_bits(),
                    6f32.to_bits(),
                    0.5f32.to_bits(),
                    1f32.to_bits(),
                    0x80ff0000,
                    0xff004000,
                ],
                1,
                1,
            ),
            [0, 64, 0, 128],
        );
    }
    // spec: GFX-FIXED-2
    #[test]
    fn blend_factor_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        for (state, value) in [(137, 0), (27, 1), (19, 14), (20, 1), (193, 0xff804020)] {
            assert_eq!(set_render(&mut e, this, state, value), 0);
        }
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[0, 0, 0.5f32.to_bits(), 0xffffffff],
                1,
                1,
            ),
            [32, 64, 128, 255],
        );
    }
    // spec: GFX-FIXED-2
    #[test]
    fn srgb_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        assert_eq!(set_render(&mut e, this, 137, 0), 0);
        assert_eq!(set_render(&mut e, this, 194, 1), 0);
        // sRGB encodes linear 64/255 as approximately 137/255.
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[0, 0, 0.5f32.to_bits(), 0xff404040],
                1,
                1,
            ),
            [137, 137, 137, 255],
        );
    }
    // spec: GFX-FIXED-2
    #[test]
    fn scaled_points_cover_expected_pixels() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        for (state, value) in [(137, 0), (157, 1), (158, 4f32.to_bits())] {
            assert_eq!(set_render(&mut e, this, state, value), 0);
        }
        // Viewport height 12 * size 1 / sqrt(A=4) = 6 pixels.
        close(
            pixel(
                &mut e,
                p,
                &target,
                0x42,
                &[0, 0, 0.5f32.to_bits(), 0xffff0000],
                1,
                1,
            ),
            [0, 0, 255, 255],
        );
        let mut read = e
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&target.image, None)
            .unwrap();
        read.take_completion_job().unwrap()();
        let pixels = read.poll().unwrap().unwrap();
        let red = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[2] == 255)
            .count();
        assert_eq!(red, 36);
    }
    // spec: GFX-FIXED-2
    #[test]
    fn depth_bias_changes_depth_comparison() {
        let (mut m, p, target) = fixture();
        let mut e = env(&mut m);
        let this = GuestPtr::new(p);
        let depth = e
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .depth(16, 12, crate::d3d9::depth::format(80).unwrap())
            .unwrap();
        let attached = target.clone().with_depth(Some(depth.clone())).unwrap();
        e.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .clear_with_stencil(&attached, Some([0., 0., 0., 1.]), Some(0.6), None)
            .unwrap();
        let owner = com(&e).identity::<Payload>(&e.sys.objects, p).unwrap();
        let (_, id) = object(
            &mut e,
            "IDirect3DSurface9",
            resources::IID_SURFACE,
            Payload::Resource(Resource::new_depth(
                owner,
                adv_abi::types::D3DSURFACE_DESC {
                    Format: 80,
                    Type: 1,
                    Usage: 2,
                    Width: 16,
                    Height: 12,
                    ..Default::default()
                },
                depth,
            )),
        )
        .unwrap();
        assert!(com(&e).retain_internal(&mut e.sys.objects, id));
        com(&e)
            .release_identity::<Payload>(&mut e.sys.objects, id)
            .unwrap();
        if let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner) {
            d.depth_stencil = Some(id);
        }
        for (state, value) in [(137, 0), (7, 1), (23, 2), (195, 0.25f32.to_bits())] {
            assert_eq!(set_render(&mut e, this, state, value), 0);
        }
        let words = [0, 0, 0.5f32.to_bits(), 0xffff0000];
        close(
            pixel(&mut e, p, &target, 0x42, &words, 1, 1),
            [0, 0, 0, 255],
        );
        assert_eq!(set_render(&mut e, this, 195, 0), 0);
        close(
            pixel(&mut e, p, &target, 0x42, &words, 1, 1),
            [0, 0, 255, 255],
        );
    }
    // spec: GFX-FIXED-1, GFX-FIXED-2, GFX-STATE-1
    #[test]
    fn fixed_render_values_roundtrip() {
        let mut m = crate::d3d9::tests::machine();
        let mut e = env(&mut m);
        let (p, _, _, _) = crate::d3d9::tests::setup(&mut e);
        let this = GuestPtr::new(p);
        let out = store(&mut e, 0u32);
        for (state, value) in [
            (48, 1),
            (60, 0x12345678),
            (136, 0),
            (142, 0),
            (143, 1),
            (157, 1),
            (158, 2f32.to_bits()),
            (159, 0.5f32.to_bits()),
            (160, 0.25f32.to_bits()),
            (162, 0x12345678),
            (165, 1),
            (170, 0.5f32.to_bits()),
            (172, 1),
            (173, 3),
            (175, 0.5f32.to_bits()),
            (176, 1),
            (190, 7),
            (191, 3),
            (192, 1),
            (193, 0x12345678),
            (194, 1),
            (195, 0.25f32.to_bits()),
        ] {
            assert_eq!(set_render(&mut e, this, state, value), 0, "state={state}");
            assert_eq!(get_render(&mut e, this, state, out), 0);
            assert_eq!(out.read(e.cpu).unwrap(), value);
        }
        for state in 198..=205 {
            assert_eq!(get_render(&mut e, this, state, out), 0);
            assert_eq!(out.read(e.cpu).unwrap(), 0);
            assert_eq!(set_render(&mut e, this, state, 1), UNAVAILABLE);
        }
    }
}

mod fixed_completion_lighting {
    use super::*;
    use lighting::*;
    // spec: GFX-FIXED-1
    #[test]
    fn spot_falloff_and_range() {
        let mut f = Fixed::new(16, 12);
        let mut l = Light::default();
        l.enabled = true;
        l.value[0] = 2;
        l.value[19] = 4f32.to_bits();
        l.value[20] = 2f32.to_bits();
        l.value[21] = 1f32.to_bits();
        l.value[24] = 0f32.to_bits();
        l.value[25] = (std::f32::consts::PI * 2. / 3.).to_bits();
        f.lights.insert(99, l);
        let lights = active(&f).ok().unwrap();
        let matrix = normal_matrix(&f);
        let shade = |eye, normal| illuminate(&f, &lights, eye, normal, &matrix, None, None).0;
        assert_eq!(shade([0., 0., 1., 1.], [0., 0., -1.]), [1.; 4]);
        // cos(theta/2)=1, cos(phi/2)=.5, cos(vertex angle)=.75.
        // Spotlight factor ((.75-.5)/(1-.5))^2 = .25; N dot L = 1.
        let x = (1f32 - 0.75f32.powi(2)).sqrt();
        let result = shade([x, 0., 0.75, 1.], [-x, 0., -0.75]);
        assert!((result[0] - 0.25).abs() < 1e-5);
        assert_eq!(shade([0., 0., 5., 1.], [0., 0., -1.]), [0., 0., 0., 1.]);
        assert_eq!(shade([1., 0., 0., 1.], [-1., 0., 0.]), [0., 0., 0., 1.]);
    }
    // spec: GFX-FIXED-1
    #[test]
    fn normal_inverse_transpose_and_normalization() {
        let mut f = Fixed::new(16, 12);
        let mut l = Light::default();
        l.enabled = true;
        f.lights.insert(0, l);
        f.transforms[0][10] = 2.;
        let lights = active(&f).ok().unwrap();
        let matrix = normal_matrix(&f);
        let normal = [0., 0., -1.];
        let eye = [0., 0., 1., 1.];
        assert_eq!(
            illuminate(&f, &lights, eye, normal, &matrix, None, None).0,
            [0.5, 0.5, 0.5, 1.]
        );
        f.render.insert(143, 1);
        assert_eq!(
            illuminate(&f, &lights, eye, normal, &matrix, None, None).0,
            [1.; 4]
        );
    }
    // spec: GFX-FIXED-1
    #[test]
    fn material_sources_and_colorvertex() {
        for state in 145..=148 {
            let mut f = Fixed::new(16, 12);
            f.material = [0.; 17];
            for state in 145..=148 {
                f.render.insert(state, 0);
            }
            let offset = match state {
                145 => 0,
                146 => 8,
                147 => 4,
                _ => 12,
            };
            f.material[offset..offset + 4].fill(0.25);
            f.material[16] = 1.;
            f.render.insert(29, 1);
            f.render.insert(142, 0);
            f.render.insert(139, u32::MAX);
            let mut l = Light::default();
            l.enabled = true;
            if state == 146 {
                l.value[5..8].fill(1f32.to_bits());
            }
            f.lights.insert(0, l);
            let lights = active(&f).ok().unwrap();
            let matrix = normal_matrix(&f);
            for (colorvertex, source, expected) in [
                (1, 0, 0.25),
                (1, 1, 0.5),
                (1, 2, 0.75),
                (0, 1, 0.25),
                (0, 2, 0.25),
            ] {
                f.render.insert(141, colorvertex);
                f.render.insert(state, source);
                let (diffuse, specular) = illuminate(
                    &f,
                    &lights,
                    [0., 0., 1., 1.],
                    [0., 0., -1.],
                    &matrix,
                    Some([0.5; 4]),
                    Some([0.75; 4]),
                );
                let result = if state == 146 { specular } else { diffuse };
                assert_eq!(
                    result[0], expected,
                    "state={state}, COLORVERTEX={colorvertex}, source={source}"
                );
            }
        }
    }
    // spec: GFX-FIXED-1
    #[test]
    fn specular_local_viewer() {
        let mut f = Fixed::new(16, 12);
        f.material[8..11].fill(1.);
        f.material[16] = 2.;
        f.render.insert(29, 1);
        f.render.insert(142, 0);
        let mut l = Light::default();
        l.enabled = true;
        l.value[5..8].fill(1f32.to_bits());
        f.lights.insert(0, l);
        let lights = active(&f).ok().unwrap();
        let matrix = normal_matrix(&f);
        let spec = |f: &Fixed| {
            illuminate(
                f,
                &lights,
                [1., 0., 0., 1.],
                [0., 0., -1.],
                &matrix,
                None,
                None,
            )
            .1
        };
        assert_eq!(spec(&f), [1., 1., 1., 0.]);
        f.render.insert(142, 1);
        assert!((spec(&f)[0] - 0.5).abs() < 1e-6);
        f.render.insert(29, 0);
        assert_eq!(spec(&f), [0.; 4]);
    }
    // spec: GFX-FIXED-2
    #[test]
    fn range_fog_and_supplied_factor() {
        let mut f = Fixed::new(16, 12);
        f.render.insert(140, 3);
        f.render.insert(37, 10f32.to_bits());
        assert_eq!(fog(&f, false, 0., [3., 0., 4., 1.], 0.25), 0.6);
        f.render.insert(48, 1);
        assert_eq!(fog(&f, false, 0., [3., 0., 4., 1.], 0.25), 0.5);
        assert_eq!(fog(&f, true, 0.5, [0.; 4], 0.25), 0.25);
        f.render.insert(35, 3);
        assert_eq!(fog(&f, true, 5., [0.; 4], 0.25), 0.5);
        assert_eq!(fog(&f, false, 0., [3., 0., 4., 1.], 0.25), 0.6);
    }
}
