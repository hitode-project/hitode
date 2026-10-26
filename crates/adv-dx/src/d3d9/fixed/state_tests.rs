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
        use super::super::shaders::{self, stateblocks};
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
            assert_eq!(
                shaders::set_constants_direct(&mut env, pointer, 94, 3, &[0; 16], 1),
                0
            );
        }
        assert_eq!(device(&env, pointer).unwrap().versions, initial);
        assert_eq!(stateblocks::begin(&mut env, this), 0);
        assert_eq!(call(&mut env, set_render_ABI, &[pointer, 22, 3]), 0);
        assert_eq!(
            shaders::set_constants_direct(&mut env, pointer, 94, 3, &[0; 16], 1),
            0
        );
        assert_eq!(stateblocks::end(&mut env, this, GuestPtr::new(DATA)), 0);
        let block = GuestPtr::new(env.cpu.read_u32(DATA).unwrap());
        assert_eq!(device(&env, pointer).unwrap().versions, initial);
        let bits = [0x8000_0000u32, 0x7fc0_1234, 0, 0];
        assert_eq!(
            shaders::set_constants_direct(&mut env, pointer, 94, 3, bytemuck::cast_slice(&bits), 1),
            0
        );
        let changed = device(&env, pointer).unwrap().versions;
        assert_eq!(changed[13], initial[13] + 1);
        assert_eq!(
            shaders::set_constants_direct(&mut env, pointer, 94, 3, bytemuck::cast_slice(&bits), 1),
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
        assert_eq!(
            super::super::shaders::set_constants_direct(&mut env, pointer, 94, 0, &[0x3f; 16], 1),
            0
        );
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
            (136, 0, UNAVAILABLE),
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
        assert_eq!(diagnostics.warnings.len(), 6);
        assert_eq!(diagnostics.warnings.values().sum::<u64>(), unavailable);
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(diagnostics.acceptance_eligible(RunMode::Strict));
        let trace = String::from_utf8(capture.0.borrow().clone()).unwrap();
        assert_eq!(trace.lines().count(), 6);
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
