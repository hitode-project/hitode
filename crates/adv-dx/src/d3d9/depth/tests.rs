//! Common COM and attachment boundaries, independent of guest fixture pixels.
use super::*;
#[test]
fn binding_public_zero_getter_parent_retain_failures_and_final_release_are_one_common_owner() {
    let mut machine = super::super::tests::machine();
    let gpu = pollster::block_on(Gpu::request(true)).expect("software depth adapter");
    let image = gpu.depth(16, 12, DepthFormat::Depth24Stencil8).unwrap();
    let equal = gpu.target(16, 12, false).unwrap();
    let smaller = gpu.target(8, 6, false).unwrap();
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (device, device_id, surface, id) = super::super::tests::setup(&mut env);
    if let Some(Payload::Resource(resource)) = com(&env).get_identity_mut(&mut env.sys.objects, id)
    {
        resource.depth = Some(image.clone());
        resource.desc.Format = 75;
        resource.desc.Pool = 0;
        resource.desc.Usage = 2;
        resource.desc.Width = 16;
        resource.desc.Height = 12;
    }
    assert_eq!(
        set(&mut env, GuestPtr::new(device), GuestPtr::new(surface)),
        0
    );
    release_binding(&mut env, Some(id)); // transfer the fixture's owner to binding
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
    assert!(attach(&env, device, equal).unwrap().depth().is_some());
    assert!(
        matches!(attach(&env, device, smaller), Err(UNAVAILABLE)),
        "larger D3D depth is explicitly outside the equal-view backend subset"
    );
    assert!(has_stencil(&env, device));
    let output = GuestPtr::<u32>::new(env.sys.vm.alloc(env.cpu, 4096, Prot::RW).unwrap());
    assert_eq!(get(&mut env, GuestPtr::new(device), output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), surface);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(release(&mut env, surface), 0);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
    assert_eq!(
        com(&env).add_ref_identity(&mut env.sys.objects, device_id),
        Some(2)
    );
    assert_eq!(get(&mut env, GuestPtr::new(device), output), OUT_OF_MEMORY);
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    com(&env)
        .release_identity(&mut env.sys.objects, device_id)
        .unwrap();
    env.cpu.protect(output.addr(), 4096, Prot::READ).unwrap();
    assert_eq!(get(&mut env, GuestPtr::new(device), output), INVALID);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    assert_eq!(
        get(&mut env, GuestPtr::new(device), GuestPtr::new(u32::MAX - 1)),
        INVALID
    );
    env.cpu.protect(output.addr(), 4096, Prot::RW).unwrap();
    if let Some(Payload::Resource(resource)) = com(&env).get_identity_mut(&mut env.sys.objects, id)
    {
        resource.desc.Format = 77;
    }
    assert!(
        !has_stencil(&env, device),
        "D24X8 does not expose the physical backend's stencil component"
    );
    let (_foreign, foreign_id, foreign_surface, foreign) = super::super::tests::setup(&mut env);
    if let Some(Payload::Resource(resource)) =
        com(&env).get_identity_mut(&mut env.sys.objects, foreign)
    {
        resource.depth = Some(image);
        resource.desc.Pool = 0;
    }
    assert_ne!(foreign_id, device_id);
    assert_eq!(
        set(
            &mut env,
            GuestPtr::new(device),
            GuestPtr::new(foreign_surface)
        ),
        INVALID
    );
    assert_eq!(super::device(&env, device).unwrap().depth_stencil, Some(id));
    assert_eq!(set(&mut env, GuestPtr::new(device), GuestPtr::new(0)), 0);
    assert!(com(&env).get_identity(&env.sys.objects, id).is_none());
    assert_eq!(get(&mut env, GuestPtr::new(device), output), 0x88760866);
    assert_eq!(output.read(env.cpu).unwrap(), 0);
}
