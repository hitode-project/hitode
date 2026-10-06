// Native oracle: target/engine-loop/20261006-himawari/stencil-depth-native.
// D16/D24X8/no attachment ignore STENCILFUNC=NEVER; D24S8 rejects fragments.
struct StencilTarget(Target);

#[test]
fn typed_unknown_render_state_warns_once_and_counts_repeats_without_changing_result() {
    use adv_cpu::backend::{Guest, Reg};
    use std::{cell::RefCell, io::Write, rc::Rc};
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
    let mut machine = super::super::tests::machine();
    const STACK: u32 = 0x550000;
    machine.cpu.map(STACK, 0x1000, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, STACK);
    let capture = Capture::default();
    machine.sys.trace = adv_machine::trace::Trace::to_writer(Box::new(capture.clone()));
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (pointer, owner, _, _) = super::super::tests::setup(&mut env);
    let count = env.sys.objects.object_count();
    let refs = env.sys.objects.strong_count(owner);
    for (offset, word) in [0x12345678, pointer, 0x7fffffff, 1].into_iter().enumerate() {
        env.cpu.write_u32(STACK + offset as u32 * 4, word).unwrap();
    }
    for value in 0..151 {
        env.cpu.write_u32(STACK + 12, value).unwrap();
        assert!(matches!(
            (set_render_ABI.handler)(&mut env),
            Outcome::Return(UNAVAILABLE)
        ));
    }
    env.cpu.write_u32(STACK + 8, 9).unwrap();
    env.cpu.write_u32(STACK + 12, 2).unwrap();
    assert!(matches!(
        (set_render_ABI.handler)(&mut env),
        Outcome::Return(0)
    ));
    env.cpu.write_u32(STACK + 8, 22).unwrap();
    env.cpu.write_u32(STACK + 12, 0).unwrap();
    assert!(matches!(
        (set_render_ABI.handler)(&mut env),
        Outcome::Return(INVALID)
    ));
    let diagnostics = env.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), repetitions) = diagnostics.warnings.first_key_value().unwrap();
    assert_eq!(api, "d3d9.dll!IDirect3DDevice9::SetRenderState");
    assert!(reason.contains("HRESULT=0x8876086a"));
    assert!(reason.contains("State=0x7fffffff"));
    assert_eq!(*repetitions, 151);
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.acceptance_eligible(adv_abi::RunMode::Strict));
    let trace = String::from_utf8(capture.0.borrow().clone()).unwrap();
    assert_eq!(trace.lines().count(), 1);
    assert!(
        trace.starts_with("# warning\t0\t0\t12345678\td3d9.dll!IDirect3DDevice9::SetRenderState\t")
    );
    assert_eq!(env.sys.objects.object_count(), count);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
}

#[test]
fn rejected_up_draw_records_the_exact_phase_without_changing_com_ownership() {
    let mut machine = super::super::tests::machine();
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (pointer, owner, _, _) = super::super::tests::setup(&mut env);
    assert_eq!(set_fvf(&mut env, GuestPtr::new(pointer), 0x144), 0);
    let count = env.sys.objects.object_count();
    let refs = env.sys.objects.strong_count(owner);
    for _ in 0..2 {
        assert!(matches!(
            draw_up(&mut env, GuestPtr::new(pointer), 4, 1, GuestPtr::new(0), 28),
            Outcome::Return(INVALID)
        ));
    }
    let diagnostics = env.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), repetitions) = diagnostics.warnings.first_key_value().unwrap();
    assert_eq!(api, "IDirect3DDevice9::DrawPrimitiveUP");
    assert!(reason.starts_with("draw outside BeginScene/EndScene;"));
    assert!(reason.contains("FVF=0x144"));
    assert!(reason.contains("stride=28"));
    assert_eq!(*repetitions, 2);
    assert_eq!(env.sys.objects.object_count(), count);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
    assert!(env.ext.get::<State>().pending.is_empty());
}

#[test]
fn depth_only_stencil_never_is_ignored_but_real_stencil_and_depth_tests_still_reject_pixels() {
    let mut gpu = pollster::block_on(Gpu::request(true)).expect("software drawing adapter");
    for (format, z_never, expected) in [
        (80, false, [0, 0, 255, 255]),
        (77, false, [0, 0, 255, 255]),
        (75, false, [0, 0, 0, 255]),
        (0, false, [0, 0, 255, 255]),
        (80, true, [0, 0, 0, 255]),
    ] {
        let mut machine = super::super::tests::machine();
        let color = gpu.target(16, 12, false).unwrap();
        let depth = (format != 0).then(|| {
            gpu.depth(16, 12, super::super::depth::format(format).unwrap())
                .unwrap()
        });
        let target = color.clone().with_depth(depth.clone()).unwrap();
        gpu.clear_with_stencil(
            &target,
            Some([0., 0., 0., 1.]),
            depth.as_ref().map(|_| 1.),
            (format == 75).then_some(0),
        )
        .unwrap();
        let draws_before = gpu.stats().draws;
        machine.ext.insert(StencilTarget(color));
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let (pointer, owner, _, back) = super::super::tests::setup(&mut env);
        let factory = device(&env, pointer).unwrap().factory;
        let factory = com(&env).canonical(&env.sys.objects, factory).unwrap();
        if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, back) {
            r.surface = Some(1);
            r.desc.Format = 21;
            r.desc.Pool = 0;
            r.desc.Width = 16;
            r.desc.Height = 12;
        }
        let depth = depth.map(|depth| {
            let (_, id) = object(
                &mut env,
                "IDirect3DSurface9",
                resources::IID_SURFACE,
                Payload::Resource(Resource::new_depth(
                    owner,
                    adv_abi::types::D3DSURFACE_DESC {
                        Format: format,
                        Type: 1,
                        Usage: 2,
                        Pool: 0,
                        Width: 16,
                        Height: 12,
                        ..Default::default()
                    },
                    depth,
                )),
            )
            .unwrap();
            assert!(com(&env).retain_internal(&mut env.sys.objects, id));
            com(&env)
                .release_identity::<Payload>(&mut env.sys.objects, id)
                .unwrap();
            id
        });
        if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
            d.scene = true;
            d.render_target = Some(back);
            d.depth_stencil = depth;
        }
        env.ext.get_mut::<State>().provider.target =
            |ext, key| (key == 1).then(|| ext.get::<StencilTarget>().0.clone());
        env.ext.get_mut::<State>().gpu = Some(gpu);
        let this = GuestPtr::new(pointer);
        assert_eq!(set_fvf(&mut env, this, 0x144), 0);
        assert_eq!(set_render(&mut env, this, 22, 1), 0); // no culling
        assert_eq!(set_render(&mut env, this, 52, 1), 0); // stencil enabled
        assert_eq!(set_render(&mut env, this, 56, 1), 0); // stencil NEVER
        assert_eq!(set_render(&mut env, this, 7, u32::from(z_never)), 0);
        assert_eq!(set_render(&mut env, this, 23, 1), 0); // depth NEVER
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(get_render(&mut env, this, 52, output), 0);
        assert_eq!(
            output.read(env.cpu).unwrap(),
            1,
            "requested state stays observable"
        );
        let bytes: Vec<_> = [(-0.5f32, -0.5f32), (15.5, -0.5), (-0.5, 11.5), (15.5, 11.5)]
            .into_iter()
            .flat_map(|(x, y)| {
                [
                    x.to_bits(),
                    y.to_bits(),
                    0.5f32.to_bits(),
                    1.0f32.to_bits(),
                    0xffff0000,
                    0,
                    0,
                ]
                .into_iter()
                .flat_map(u32::to_le_bytes)
            })
            .collect();
        let vertices = alloc(&mut env, bytes.len() as u32).unwrap();
        env.cpu.write(vertices, &bytes).unwrap();
        let aliases = env.sys.objects.alias_count(com(&env).namespace());
        let refs = env.sys.objects.strong_count(owner);
        assert!(
            matches!(
                draw_up(&mut env, this, 5, 2, GuestPtr::new(vertices), 28),
                Outcome::Return(0)
            ),
            "format={format}, z_never={z_never}"
        );
        assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
        assert_eq!(env.sys.objects.strong_count(owner), refs);
        assert!(env.ext.get::<State>().pending.is_empty());
        let mut transfer = env
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&target.image, None)
            .unwrap();
        std::thread::spawn(transfer.take_completion_job().unwrap())
            .join()
            .unwrap();
        let pixels = transfer.poll().expect("completed readback").unwrap();
        assert_eq!(pixels.len(), 16 * 12 * 4);
        for pixel in pixels.chunks_exact(4) {
            assert_eq!(pixel, expected, "format={format}, z_never={z_never}");
        }
        assert_eq!(
            env.ext.get::<State>().gpu.as_ref().unwrap().stats().draws,
            draws_before + 1
        );
        free(&mut env, vertices);
        free(&mut env, output.addr());
        assert_eq!(release(&mut env, pointer), 0);
        assert_eq!(release(&mut env, factory), 0);
        assert_eq!(env.sys.objects.object_count(), 0);
        gpu = env.ext.get_mut::<State>().gpu.take().unwrap();
    }
}
