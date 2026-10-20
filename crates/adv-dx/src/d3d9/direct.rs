//! Host-side calls from D3DX Effect passes into the device (GFX-EFFECT-3).
//! A vtable entry that still holds the published thunk runs its
//! implementation directly; a replaced (hooked) entry keeps the guest call.
use super::*;

/// Run `pointer`'s method `slot`, whose live vtable entry is `entry`, with
/// `args` after `this`. `data` stands in for the guest buffer of a constant
/// upload. None leaves the call to the guest vtable.
pub(crate) fn effect_call(
    env: &mut Env<'_>,
    pointer: u32,
    slot: u32,
    entry: u32,
    args: &[u32],
    data: Option<&[u32]>,
) -> Option<Outcome> {
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    let this = GuestPtr::new(pointer);
    let state = env.ext.try_get::<State>()?;
    let (device, block) = match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Device(_) => (true, false),
        Payload::StateBlock(_) => (false, true),
        _ => return None,
    };
    let methods = if device {
        &state.device_methods
    } else {
        &state.block_methods
    };
    if methods.get(slot as usize) != Some(&entry) {
        return None;
    }
    let value = match (block, slot) {
        (true, 4) => shaders::stateblocks::capture(env, this),
        (true, 5) => shaders::stateblocks::apply(env, this),
        (false, 57) => fixed::set_render(env, this, a(0) as i32, a(1)),
        (false, 65) => return Some(resources::set_texture(env, this, a(0), GuestPtr::new(a(1)))),
        (false, 67) => fixed::set_stage(env, this, a(0), a(1) as i32, a(2)),
        (false, 69) => fixed::set_sampler(env, this, a(0), a(1) as i32, a(2)),
        (false, 89) => fixed::set_fvf(env, this, a(0)),
        (false, 92) => shaders::set_vertex(env, this, GuestPtr::new(a(0))),
        (false, 107) => shaders::set_pixel(env, this, GuestPtr::new(a(0))),
        (false, 94 | 96 | 98 | 109 | 111 | 113) => shaders::set_constants_direct(
            env,
            pointer,
            slot,
            a(0),
            bytemuck::cast_slice(data?),
            a(2),
        ),
        _ => return None,
    };
    Some(Outcome::Return(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::d3d9::stretch::tests::Fixture;
    use crate::shader::Stage;

    fn env(m: &mut Machine<adv_cpu::JitCpu>) -> Env<'_> {
        Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        }
    }

    #[test]
    // spec: GFX-EFFECT-3
    fn effect_calls_run_published_methods_and_leave_replaced_entries_to_the_guest() {
        let mut f = Fixture::new();
        let d = f.device;
        let (render, constants) = (0x7e00_1000, 0x7e00_2000);
        let mut methods = vec![0; 120];
        methods[57] = render;
        methods[94] = constants;
        f.m.ext.get_mut::<State>().device_methods = methods;
        let versions = |m: &mut Machine<adv_cpu::JitCpu>| device(&env(m), d).unwrap().versions;
        let before = versions(&mut f.m);
        // A hooked entry keeps the guest call and changes nothing.
        assert!(effect_call(&mut env(&mut f.m), d, 57, render + 4, &[27, 1], None).is_none());
        assert_eq!(versions(&mut f.m), before);
        assert!(matches!(
            effect_call(&mut env(&mut f.m), d, 57, render, &[27, 1], None),
            Some(Outcome::Return(0))
        ));
        assert_ne!(versions(&mut f.m), before);
        // Constants come from host data; without it the guest buffer is used.
        let value = [1.5f32, -2.0, 0.25, 4.0].map(f32::to_bits);
        assert!(effect_call(&mut env(&mut f.m), d, 94, constants, &[3, 0, 1], None).is_none());
        assert!(matches!(
            effect_call(
                &mut env(&mut f.m),
                d,
                94,
                constants,
                &[3, 0, 1],
                Some(&value)
            ),
            Some(Outcome::Return(0))
        ));
        let stored = device(&env(&mut f.m), d)
            .unwrap()
            .shaders
            .constants(Stage::Vertex)
            .floats[3];
        assert_eq!(stored.map(f32::to_bits), value);
        // Out-of-range registers fail like the API.
        assert!(matches!(
            effect_call(
                &mut env(&mut f.m),
                d,
                94,
                constants,
                &[256, 0, 1],
                Some(&value)
            ),
            Some(Outcome::Return(INVALID))
        ));
    }
}
