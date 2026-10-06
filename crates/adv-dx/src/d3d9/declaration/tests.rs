use super::*;

fn setup(env: &mut Env<'_>) -> (u32, Id) {
    let (device, id, _, resource) = super::super::tests::setup(env);
    let dead = com(env)
        .release_internal(&mut env.sys.objects, resource)
        .unwrap()
        .unwrap();
    super::super::destroy(env, dead);
    (device, id)
}
/// D3DVERTEXELEMENT9 records: (stream, offset, type, usage, usage index).
fn elements(env: &mut Env<'_>, list: &[(u16, u16, u8, u8, u8)]) -> u32 {
    let mut bytes = Vec::new();
    for &(stream, offset, kind, usage, index) in list {
        bytes.extend(stream.to_le_bytes());
        bytes.extend(offset.to_le_bytes());
        bytes.extend([kind, 0, usage, index]);
    }
    bytes.extend([0xff, 0, 0, 0, 17, 0, 0, 0]);
    let pointer = alloc(env, bytes.len() as u32).unwrap();
    env.cpu.write(pointer, &bytes).unwrap();
    pointer
}
fn fvf(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let output = GuestPtr::<u32>::new(alloc(env, 4).unwrap());
    assert_eq!(fixed::get_fvf(env, this, output), 0);
    output.read(env.cpu).unwrap()
}
const SPRITE: &[(u16, u16, u8, u8, u8)] = &[(0, 0, 3, 9, 0), (0, 16, 4, 10, 0), (0, 20, 1, 5, 0)];

#[test]
fn creation_copies_terminated_elements_and_holds_the_device_while_public() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, id) = setup(&mut env);
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let input = elements(&mut env, SPRITE);
    assert_eq!(
        create(
            &mut env,
            GuestPtr::new(device),
            GuestPtr::new(input),
            output
        ),
        0
    );
    let declaration = output.read(env.cpu).unwrap();
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(2));
    // The count includes D3DDECL_END and a null element pointer queries it.
    let count = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let this = GuestPtr::new(declaration);
    assert_eq!(
        declaration_elements(&mut env, this, GuestPtr::new(0), count),
        0
    );
    assert_eq!(count.read(env.cpu).unwrap(), 4);
    let copy = alloc(&mut env, 32).unwrap();
    assert_eq!(
        declaration_elements(&mut env, this, GuestPtr::new(copy), count),
        0
    );
    let (mut expected, mut actual) = ([0; 32], [0; 32]);
    env.cpu.read(input, &mut expected).unwrap();
    env.cpu.read(copy, &mut actual).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        declaration_elements(&mut env, this, GuestPtr::new(copy), GuestPtr::new(0)),
        INVALID
    );
    // The fixture caps references at three; the declaration alone keeps the
    // device alive once the creator's reference is gone.
    assert_eq!(release(&mut env, device), 1);
    assert_eq!(declaration_device(&mut env, this, output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), device);
    assert_eq!(release(&mut env, device), 1);
    assert_eq!(release(&mut env, declaration), 0);
    assert!(com(&env).get_identity(&env.sys.objects, id).is_none());
}

#[test]
fn malformed_declarations_fail_without_publishing_or_retaining_the_device() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, id) = setup(&mut env);
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let count = env.sys.objects.alias_count(com(&env).namespace());
    let unterminated = alloc(&mut env, 8 * 66).unwrap();
    for (stream, offset, kind, usage) in [(16, 0, 2, 0), (0, 2, 2, 0), (0, 0, 17, 0), (0, 0, 2, 14)]
    {
        let input = elements(&mut env, &[(stream, offset, kind, usage, 0)]);
        assert_eq!(
            create(
                &mut env,
                GuestPtr::new(device),
                GuestPtr::new(input),
                output
            ),
            INVALID
        );
    }
    for input in [0, unterminated] {
        assert_eq!(
            create(
                &mut env,
                GuestPtr::new(device),
                GuestPtr::new(input),
                output
            ),
            INVALID
        );
    }
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), count);
}

#[test]
fn binding_replaces_the_fvf_and_set_fvf_releases_the_declaration() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, _) = setup(&mut env);
    let this = GuestPtr::new(device);
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let input = elements(&mut env, SPRITE);
    assert_eq!(create(&mut env, this, GuestPtr::new(input), output), 0);
    let declaration = output.read(env.cpu).unwrap();
    let decl = com(&env)
        .identity::<Payload>(&env.sys.objects, declaration)
        .unwrap();
    assert_eq!(fixed::set_fvf(&mut env, this, 0x144), 0);
    assert_eq!(set(&mut env, this, GuestPtr::new(declaration)), 0);
    assert_eq!(fvf(&mut env, this), 0);
    assert_eq!(fixed::vertex_size(&env, device), Ok(28));
    // The device binding alone keeps the declaration alive.
    assert_eq!(release(&mut env, declaration), 0);
    assert!(com(&env).get_identity(&env.sys.objects, decl).is_some());
    assert_eq!(get(&mut env, this, output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), declaration);
    assert_eq!(release(&mut env, declaration), 0);
    assert_eq!(set(&mut env, this, GuestPtr::new(device)), INVALID);
    assert_eq!(fixed::set_fvf(&mut env, this, 0x142), 0);
    assert_eq!(fvf(&mut env, this), 0x142);
    assert!(com(&env).get_identity(&env.sys.objects, decl).is_none());
    assert_eq!(get(&mut env, this, output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), 0);
}

#[test]
fn only_fvf_equivalent_stream_zero_inputs_map_onto_the_fixed_pipeline() {
    let encode = |list: &[(u16, u16, u8, u8, u8)]| -> Vec<[u8; 8]> {
        list.iter()
            .map(|&(stream, offset, kind, usage, index)| {
                let [s0, s1] = stream.to_le_bytes();
                let [o0, o1] = offset.to_le_bytes();
                [s0, s1, o0, o1, kind, 0, usage, index]
            })
            .chain([[0xff, 0, 0, 0, 17, 0, 0, 0]])
            .collect()
    };
    assert_eq!(
        fixed_layout(&encode(SPRITE)),
        Ok(fixed::Layout::from_fvf(0x144).unwrap())
    );
    let permuted = fixed_layout(&encode(&[
        (0, 0, 1, 5, 0),
        (0, 8, 4, 10, 0),
        (0, 12, 2, 0, 0),
    ]));
    assert_eq!(
        permuted,
        Ok(fixed::Layout {
            transformed: false,
            position: 12,
            color: Some(8),
            uv: Some(0),
            size: 24,
        })
    );
    for (list, reason) in [
        (&[(1, 0, 2, 0, 0)][..], "stream other than 0"),
        (
            &[(0, 0, 2, 0, 0), (0, 12, 2, 3, 0)][..],
            "no fixed-function equivalent",
        ),
        (
            &[(0, 0, 2, 0, 0), (0, 12, 1, 5, 1)][..],
            "no fixed-function equivalent",
        ),
        (&[(0, 0, 4, 10, 0)][..], "no POSITION"),
    ] {
        let error = fixed_layout(&encode(list)).unwrap_err();
        assert!(error.contains(reason), "{error}");
    }
}
