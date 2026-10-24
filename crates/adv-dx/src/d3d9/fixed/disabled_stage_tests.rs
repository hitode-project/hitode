mod disabled_stage_tests {
    use super::*;
    use crate::d3d9::shaders::stateblocks;

    #[test]
    fn disabled_stage_defaults_and_rejections_preserve_draw_state() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
        let original = current(&env, pointer).unwrap().stage.clone();
        for stage in 1..8 {
            for (kind, default) in [(1, 1), (2, 2), (3, 1), (4, 1), (5, 2), (6, 1)] {
                assert_eq!(get_stage(&mut env, this, stage, kind, output), 0);
                assert_eq!(output.read(env.cpu).unwrap(), default);
                let value = if matches!(kind, 1 | 4) { 1 } else { 0 };
                assert_eq!(set_stage(&mut env, this, stage, kind, value), 0);
                assert_eq!(get_stage(&mut env, this, stage, kind, output), 0);
                assert_eq!(output.read(env.cpu).unwrap(), value);
                // Stage one combines; MODULATE2X stays unavailable there.
                let unsupported = match (kind, stage) {
                    (1 | 4, 1) => 5,
                    (1 | 4, _) => 4,
                    _ => 7,
                };
                let error = if matches!(kind, 1 | 4) {
                    UNAVAILABLE
                } else {
                    INVALID
                };
                assert_eq!(set_stage(&mut env, this, stage, kind, unsupported), error);
                assert_eq!(get_stage(&mut env, this, stage, kind, output), 0);
                assert_eq!(output.read(env.cpu).unwrap(), value);
                if !matches!(kind, 1 | 4) {
                    assert_eq!(set_stage(&mut env, this, stage, kind, 0x36), 0);
                    assert_eq!(get_stage(&mut env, this, stage, kind, output), 0);
                    assert_eq!(output.read(env.cpu).unwrap(), 0x36);
                    assert_eq!(set_stage(&mut env, this, stage, kind, 0x40), INVALID);
                }
            }
        }
        for stage in 1..8 {
            assert_eq!(get_stage(&mut env, this, stage, 11, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), stage);
        }
        assert_eq!(set_stage(&mut env, this, 8, 1, 1), INVALID);
        output.write(env.cpu, 0x12345678).unwrap();
        assert_eq!(get_stage(&mut env, this, 8, 1, output), INVALID);
        assert_eq!(get_stage(&mut env, this, 2, 999, output), INVALID);
        assert_eq!(output.read(env.cpu).unwrap(), 0x12345678);
        assert_eq!(get_render(&mut env, this, 128, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(set_render(&mut env, this, 128, 0), 0);
        assert_eq!(set_render(&mut env, this, 128, 15), UNAVAILABLE);
        assert_eq!(set_render(&mut env, this, 128, 16), INVALID);
        assert_eq!(current(&env, pointer).unwrap().stage, original);
        assert_eq!(current(&env, pointer).unwrap().render[&128], 0);
        assert!(env.ext.get::<State>().pending.is_empty());
    }

    // spec: GFX-STATE-1
    #[test]
    fn wrap_defaults_and_ambient_roundtrip() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
        for state in 128..=135 {
            assert_eq!(get_render(&mut env, this, state, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), 0);
            assert_eq!(set_render(&mut env, this, state, 0), 0);
            for value in 1..=15 {
                assert_eq!(set_render(&mut env, this, state, value), UNAVAILABLE);
            }
            assert_eq!(set_render(&mut env, this, state, 16), INVALID);
            assert_eq!(get_render(&mut env, this, state, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), 0);
        }
        assert_eq!(get_render(&mut env, this, 139, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        for value in [0, 0x12345678, u32::MAX] {
            assert_eq!(set_render(&mut env, this, 139, value), 0);
            assert_eq!(get_render(&mut env, this, 139, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), value);
        }
    }

    #[test]
    fn second_stage_combines_supported_operations_and_arguments() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let second =
            |env: &Env<'_>| second_stage(current(env, pointer).unwrap()).map_err(|e| e.code);
        assert_eq!(second(&env), Ok(None));
        for op in [2, 3, 4, 7, 10] {
            assert_eq!(set_stage(&mut env, this, 1, 1, op), 0);
            assert_eq!(set_stage(&mut env, this, 1, 4, op), 0);
            assert_eq!(set_stage(&mut env, this, 2, 1, op), UNAVAILABLE);
        }
        assert_eq!(set_stage(&mut env, this, 1, 11, 3), 0);
        assert_eq!(set_stage(&mut env, this, 1, 11, 0x10001), UNAVAILABLE);
        assert_eq!(set_stage(&mut env, this, 1, 11, 8), INVALID);
        // Defaults: TEXTURE and CURRENT for both color and alpha.
        assert_eq!(
            second(&env),
            Ok(Some([10, 2 | 1 << 8, 10 | 2 << 8 | 1 << 16, 3]))
        );
        assert!(programmable::active(&env, pointer));
        // A disabled first stage ends the cascade before stage one.
        assert_eq!(set_stage(&mut env, this, 0, 1, 1), 0);
        assert_eq!(second(&env), Ok(None));
        assert!(!programmable::active(&env, pointer));
        assert_eq!(set_stage(&mut env, this, 0, 1, 4), 0);
        // Specular, temporary and modified arguments are not combined.
        for value in [4, 5, 0x12, 0x21] {
            assert_eq!(set_stage(&mut env, this, 1, 2, value), 0);
            assert_eq!(second(&env), Err(UNAVAILABLE));
            assert!(programmable::active(&env, pointer));
        }
        assert_eq!(set_stage(&mut env, this, 1, 2, 0), 0);
        assert_eq!(second(&env).unwrap().unwrap()[1], 1 << 8);
    }

    #[test]
    fn disabled_stage_arguments_follow_typed_and_recorded_state_blocks() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
        for kind in 1..=3 {
            assert_eq!(set_stage(&mut env, this, 2, 2, 2), 0);
            assert_eq!(stateblocks::create(&mut env, this, kind, output), 0);
            let block = GuestPtr::new(output.read(env.cpu).unwrap());
            assert_eq!(set_stage(&mut env, this, 2, 2, 0), 0);
            assert_eq!(stateblocks::apply(&mut env, block), 0);
            assert_eq!(get_stage(&mut env, this, 2, 2, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), if kind == 3 { 0 } else { 2 });
            release(&mut env, block.addr());
        }
        assert_eq!(set_stage(&mut env, this, 1, 3, 1), 0);
        assert_eq!(stateblocks::begin(&mut env, this), 0);
        assert_eq!(set_stage(&mut env, this, 1, 3, 0), 0);
        assert_eq!(set_stage(&mut env, this, 7, 1, 1), 0);
        assert_eq!(get_stage(&mut env, this, 1, 3, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 1);
        assert_eq!(stateblocks::end(&mut env, this, output), 0);
        let block = GuestPtr::new(output.read(env.cpu).unwrap());
        assert_eq!(set_stage(&mut env, this, 2, 3, 2), 0);
        assert_eq!(stateblocks::apply(&mut env, block), 0);
        assert_eq!(get_stage(&mut env, this, 1, 3, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(get_stage(&mut env, this, 2, 3, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 2);
        assert_eq!(set_stage(&mut env, this, 1, 3, 2), 0);
        assert_eq!(stateblocks::capture(&mut env, block), 0);
        assert_eq!(set_stage(&mut env, this, 1, 3, 0), 0);
        assert_eq!(stateblocks::apply(&mut env, block), 0);
        assert_eq!(get_stage(&mut env, this, 1, 3, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 2);
        release(&mut env, block.addr());
    }
}
