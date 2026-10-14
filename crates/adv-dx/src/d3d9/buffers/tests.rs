use super::*;
use adv_cpu::backend::Guest;

#[path = "multi_stream_tests.rs"]
mod multi_stream_tests;

fn setup(env: &mut Env<'_>) -> (u32, Id) {
    let (device, id, _, resource) = super::super::tests::setup(env);
    let dead = com(env)
        .release_internal(&mut env.sys.objects, resource)
        .unwrap()
        .unwrap();
    super::super::destroy(env, dead);
    (device, id)
}

#[test]
fn hardware_mode_rejects_software_buffer_usage_before_publishing_or_retaining_parent() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, id) = setup(&mut env);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
        d.creation = super::super::creation::State::new(0, 1, 0, 0x42).unwrap();
    }
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let count = env.sys.objects.alias_count(com(&env).namespace());
    assert_eq!(
        create_vertex(
            &mut env,
            GuestPtr::new(device),
            128,
            0x10,
            0x42,
            1,
            output,
            GuestPtr::new(0)
        ),
        INVALID
    );
    assert_eq!(
        create_index(
            &mut env,
            GuestPtr::new(device),
            12,
            0x10,
            101,
            1,
            output,
            GuestPtr::new(0)
        ),
        INVALID
    );
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), count);
}

#[test]
fn zero_public_binding_resurrects_parent_once_and_unbinding_reclaims_storage() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, device_id) = setup(&mut env);
    let out = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    assert_eq!(
        create_vertex(
            &mut env,
            GuestPtr::new(device),
            128,
            0,
            0,
            1,
            out,
            GuestPtr::new(0)
        ),
        0
    );
    let vb = out.read(env.cpu).unwrap();
    let storage = buffer(&env, vb).unwrap().storage;
    let id = com(&env).identity::<Payload>(&env.sys.objects, vb).unwrap();
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(
        set_stream(
            &mut env,
            GuestPtr::new(device),
            15,
            GuestPtr::new(vb),
            4,
            20
        ),
        0
    );
    assert_eq!(release(&mut env, vb), 0);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
    let offset = GuestPtr::new(alloc(&mut env, 4).unwrap());
    let stride = GuestPtr::new(alloc(&mut env, 4).unwrap());
    assert_eq!(
        get_stream(&mut env, GuestPtr::new(device), 15, out, offset, stride),
        0
    );
    assert_eq!(
        (
            out.read(env.cpu).unwrap(),
            offset.read(env.cpu).unwrap(),
            stride.read(env.cpu).unwrap()
        ),
        (vb, 4, 20)
    );
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(release(&mut env, vb), 0);
    assert_eq!(
        set_stream(
            &mut env,
            GuestPtr::new(device),
            15,
            GuestPtr::new(0),
            123,
            456
        ),
        0
    );
    assert!(com(&env).get_identity(&env.sys.objects, id).is_none());
    let heap = env.ext.get_mut::<Heaps>();
    let handle = heap.process_heap;
    assert_eq!(heap.size(env.cpu, handle, storage), None);
    assert_eq!(
        get_stream(&mut env, GuestPtr::new(device), 15, out, offset, stride),
        0
    );
    assert_eq!(
        (
            out.read(env.cpu).unwrap(),
            offset.read(env.cpu).unwrap(),
            stride.read(env.cpu).unwrap()
        ),
        (0, 4, 20)
    );
}

#[test]
fn invalid_lock_output_or_range_does_not_change_existing_locks_or_memory() {
    let mut m = super::super::tests::machine();
    m.cpu.map(0x550000, 4096, Prot::RW).unwrap();
    m.cpu.write_u32(0x550000, 0xaabbccdd).unwrap();
    m.cpu.protect(0x550000, 4096, Prot::READ).unwrap();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, _) = setup(&mut env);
    let out = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    assert_eq!(
        create_vertex(
            &mut env,
            GuestPtr::new(device),
            32,
            0,
            0,
            1,
            out,
            GuestPtr::new(0)
        ),
        0
    );
    let vb = out.read(env.cpu).unwrap();
    assert_eq!(lock(&mut env, vb, Kind::Vertex, 4, 0, out, 0), 0);
    let address = out.read(env.cpu).unwrap();
    assert_eq!(
        lock(&mut env, vb, Kind::Vertex, 0, 4, GuestPtr::new(0x550000), 0),
        INVALID
    );
    for (offset, size, flags) in [
        (33, 0, 0),
        (28, 5, 0),
        (u32::MAX, 2, 0),
        (0, u32::MAX, 0),
        (0, 4, 0x80000000),
    ] {
        assert_eq!(
            lock(&mut env, vb, Kind::Vertex, offset, size, out, flags),
            INVALID
        );
        assert_eq!(out.read(env.cpu).unwrap(), address);
    }
    assert_eq!(env.cpu.read_u32(0x550000).unwrap(), 0xaabbccdd);
    assert_eq!(buffer(&env, vb).unwrap().locks, 1);
    assert_eq!(unlock(&mut env, vb, Kind::Vertex), 0);
    assert_eq!(unlock(&mut env, vb, Kind::Vertex), 0);
    assert_eq!(buffer(&env, vb).unwrap().locks, 0);
}

#[test]
fn getter_validates_every_output_before_retain_or_partial_publication() {
    let mut m = super::super::tests::machine();
    m.cpu.map(0x550000, 4096, Prot::RW).unwrap();
    m.cpu.write_u32(0x550000, 0xcccccccc).unwrap();
    m.cpu.protect(0x550000, 4096, Prot::READ).unwrap();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, device_id) = setup(&mut env);
    let out = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let offset = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    assert_eq!(
        create_vertex(
            &mut env,
            GuestPtr::new(device),
            64,
            0,
            0,
            1,
            out,
            GuestPtr::new(0)
        ),
        0
    );
    let vb = out.read(env.cpu).unwrap();
    assert_eq!(
        set_stream(&mut env, GuestPtr::new(device), 0, GuestPtr::new(vb), 4, 20),
        0
    );
    assert_eq!(release(&mut env, vb), 0);
    out.write(env.cpu, 0xaaaaaaaa).unwrap();
    offset.write(env.cpu, 0xbbbbbbbb).unwrap();
    assert_eq!(
        get_stream(
            &mut env,
            GuestPtr::new(device),
            0,
            out,
            offset,
            GuestPtr::new(0x550000)
        ),
        INVALID
    );
    assert_eq!(
        (
            out.read(env.cpu).unwrap(),
            offset.read(env.cpu).unwrap(),
            env.cpu.read_u32(0x550000).unwrap()
        ),
        (0xaaaaaaaa, 0xbbbbbbbb, 0xcccccccc)
    );
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
}

#[test]
fn index_decode_preserves_signed_base_and_observes_guest_writes_without_reusing_a_snapshot() {
    let mut m = super::super::tests::machine();
    m.cpu.map(0x550000, 4096, Prot::RW).unwrap();
    m.cpu.write(0x550000, &[1, 0, 2, 0, 3, 0]).unwrap();
    let env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    assert_eq!(
        decode_indices(&env, 0x550000, 101, 3, 1, 3, -1).unwrap(),
        [0, 1, 2]
    );
    env.cpu.write(0x550000, &[3, 0]).unwrap();
    assert_eq!(
        decode_indices(&env, 0x550000, 101, 3, 1, 3, -1).unwrap(),
        [2, 1, 2]
    );
    assert!(decode_indices(&env, 0x550000, 101, 3, 1, 2, -1).is_err());
    assert!(decode_indices(&env, 0x550000, 101, 3, 1, 3, -4).is_err());
    assert!(decode_indices(&env, 0xffff_fffe, 102, 3, 0, 4, 0).is_err());
}

#[test]
fn zero_indexed_up_frees_zero_public_bindings_and_clears_offset_stride_without_gpu_work() {
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
    assert_eq!(
        create_vertex(
            &mut env,
            GuestPtr::new(device),
            64,
            0,
            0,
            1,
            output,
            GuestPtr::new(0)
        ),
        0
    );
    let vb = output.read(env.cpu).unwrap();
    let vb_id = com(&env).identity::<Payload>(&env.sys.objects, vb).unwrap();
    assert_eq!(
        set_stream(&mut env, GuestPtr::new(device), 0, GuestPtr::new(vb), 4, 20),
        0
    );
    assert_eq!(release(&mut env, vb), 0);
    assert_eq!(
        create_index(
            &mut env,
            GuestPtr::new(device),
            8,
            0,
            101,
            1,
            output,
            GuestPtr::new(0)
        ),
        0
    );
    let ib = output.read(env.cpu).unwrap();
    let ib_id = com(&env).identity::<Payload>(&env.sys.objects, ib).unwrap();
    assert_eq!(
        set_indices(&mut env, GuestPtr::new(device), GuestPtr::new(ib)),
        0
    );
    assert_eq!(release(&mut env, ib), 0);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
        d.scene = true;
    }
    assert!(matches!(
        draw_indexed_up(
            &mut env,
            GuestPtr::new(device),
            4,
            0,
            0,
            0,
            GuestPtr::new(0),
            101,
            GuestPtr::new(0),
            20
        ),
        Outcome::Return(0)
    ));
    assert!(com(&env).get_identity(&env.sys.objects, vb_id).is_none());
    assert!(com(&env).get_identity(&env.sys.objects, ib_id).is_none());
    let d = super::super::device(&env, device).unwrap();
    assert_eq!(
        (
            d.buffers.streams[0].buffer,
            d.buffers.streams[0].offset,
            d.buffers.streams[0].stride,
            d.buffers.indices
        ),
        (None, 0, 0, None)
    );
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert!(env.ext.get::<State>().gpu.is_none());
}
