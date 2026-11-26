use super::*;

fn query_face(machine: &mut Machine<JitCpu>, object: u32, iid: Iid) -> u32 {
    machine.cpu.write(IID, &iid).unwrap();
    assert_eq!(method(machine, object, 0, &[IID, OUT]), 0);
    machine.cpu.read_u32(OUT).unwrap()
}

// spec: GFX-CTX-1
#[test]
fn deferred_context_at_9_3_finishes_and_restores_state() {
    let mut machine = machine();
    let (device, immediate) = create(&mut machine, 1, 0, &[0x9300]);
    assert_eq!(method(&mut machine, device, 27, &[0, OUT]), 0);
    let deferred = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, deferred, 112, &[]), 1);
    assert_eq!(method(&mut machine, immediate, 114, &[0, OUT]), 0x887a0001);
    assert_eq!(method(&mut machine, deferred, 24, &[4]), 0);
    assert_eq!(method(&mut machine, deferred, 114, &[1, OUT]), 0);
    let list = machine.cpu.read_u32(OUT).unwrap();
    check_vtable(&mut machine, list, DLL, "ID3D11CommandList");
    assert_eq!(method(&mut machine, list, 7, &[]), 0);
    assert_eq!(method(&mut machine, deferred, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 4);
    assert_eq!(method(&mut machine, deferred, 114, &[0, OUT]), 0);
    assert_eq!(method(&mut machine, deferred, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(method(&mut machine, immediate, 24, &[5]), 0);
    assert_eq!(method(&mut machine, immediate, 58, &[list, 1]), 0);
    assert_eq!(method(&mut machine, immediate, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 5);
    assert_eq!(method(&mut machine, immediate, 58, &[list, 0]), 0);
    assert_eq!(method(&mut machine, immediate, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
}

// spec: GFX-CTX-1 GFX-INTEROP-1
#[test]
fn d3d10_face_uses_the_same_device_and_can_swap_d3d11_state() {
    let mut machine = machine();
    let levels = DATA + 0x700;
    machine.cpu.write_u32(levels, 0xa000).unwrap();
    let create = machine.resolve_import(DLL, "D3D11CreateDevice").unwrap();
    assert_eq!(
        guest_call(
            &mut machine,
            create,
            &[0, 3, 0, 0x80000020, levels, 1, 7, OUT, LEVEL, CONTEXT,]
        ),
        0
    );
    let device = machine.cpu.read_u32(OUT).unwrap();
    let context = machine.cpu.read_u32(CONTEXT).unwrap();
    let d3d10 = query_face(&mut machine, device, IID_ID3D10DEVICE1);
    assert_ne!(device, d3d10);
    check_vtable(&mut machine, d3d10, DLL, "ID3D10Device1");
    assert_eq!(refs(&mut machine, d3d10), 1);
    assert_eq!(method(&mut machine, d3d10, 100, &[]), 0xa000);
    assert_eq!(method(&mut machine, d3d10, 94, &[]), 0x20);
    assert_eq!(query_face(&mut machine, d3d10, IID_ID3D10DEVICE), d3d10);
    assert_eq!(method(&mut machine, d3d10, 2, &[]), 1);
    let dxgi = query_face(&mut machine, d3d10, IID_IDXGIDEVICE);
    assert_eq!(query_face(&mut machine, dxgi, IID_ID3D11DEVICE1), device);

    let desc = DATA + 0x760;
    let buffer = adv_abi::types::D3D10_BUFFER_DESC {
        ByteWidth: 32,
        Usage: 2,
        BindFlags: Bind::CONSTANT,
        CPUAccessFlags: 0x10000,
        MiscFlags: 0,
    };
    machine
        .cpu
        .write(desc, bytemuck::bytes_of(&buffer))
        .unwrap();
    assert_eq!(method(&mut machine, d3d10, 71, &[desc, 0, OUT]), 0);
    let buffer10 = machine.cpu.read_u32(OUT).unwrap();
    check_vtable(&mut machine, buffer10, DLL, "ID3D10Buffer");
    assert_eq!(method(&mut machine, buffer10, 10, &[4, 0, OUT]), 0);
    assert_ne!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(method(&mut machine, buffer10, 11, &[]), 0);
    let buffer11 = query_face(&mut machine, buffer10, IID_ID3D11BUFFER);
    assert_ne!(buffer10, buffer11);
    assert_eq!(refs(&mut machine, buffer10), 1);
    assert_eq!(refs(&mut machine, buffer11), 1);
    assert_eq!(method(&mut machine, buffer10, 2, &[]), 0);
    assert_eq!(method(&mut machine, buffer11, 2, &[]), 0);

    assert_eq!(method(&mut machine, d3d10, 18, &[4]), 0);
    assert_eq!(method(&mut machine, context, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 4);
    machine.cpu.write(IID, &IID_ID3D11DEVICE).unwrap();
    assert_eq!(
        method(
            &mut machine,
            device,
            47,
            &[0, levels, 1, 7, IID, LEVEL, OUT]
        ),
        0
    );
    let state = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(machine.cpu.read_u32(LEVEL).unwrap(), 0xa000);
    assert_eq!(method(&mut machine, context, 131, &[state, OUT]), 0);
    let previous = machine.cpu.read_u32(OUT).unwrap();
    assert_ne!(state, previous);
    assert_eq!(method(&mut machine, context, 83, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(method(&mut machine, context, 131, &[previous, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), state);
    assert_eq!(method(&mut machine, d3d10, 50, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 4);
}

// spec: GFX-CAP-4 GFX-INTEROP-1 PERS-EXPORT-2
#[test]
fn core_creation_exports_use_their_measured_x86_signatures() {
    let mut machine = machine();
    let core = machine
        .resolve_import(DLL, "D3D11CoreCreateDevice")
        .unwrap();
    let levels = DATA + 0x700;
    machine.cpu.write_u32(levels, 0x9300).unwrap();
    assert_eq!(
        guest_call(&mut machine, core, &[0, 0, 1, 0, 0, 0, 0, 6, OUT, LEVEL]),
        0x887a0004
    );
    assert_eq!(machine.cpu.read_u32(LEVEL).unwrap(), 0);
    assert_eq!(
        guest_call(&mut machine, core, &[0, 0, 2, 0, 0, levels, 1, 8, 0, LEVEL]),
        1
    );
    assert_eq!(machine.cpu.read_u32(LEVEL).unwrap(), 0x9300);
    assert_eq!(
        guest_call(
            &mut machine,
            core,
            &[0, 0, 2, 0, 0x400, levels, 1, 0, OUT, LEVEL]
        ),
        0x887a002d
    );
    let create10 = machine
        .resolve_import("d3d10core.dll", "D3D10CoreCreateDevice")
        .unwrap();
    assert_eq!(guest_call(&mut machine, create10, &[0; 5]), 0x80004001);
    let create10_1 = machine
        .resolve_import("d3d10_1core.dll", "D3D10CoreCreateDevice1")
        .unwrap();
    assert_eq!(guest_call(&mut machine, create10_1, &[0; 7]), 0x80004001);
    let size = machine
        .resolve_import(DLL, "D3D11CoreGetLayeredDeviceSize")
        .unwrap();
    assert_eq!(guest_call(&mut machine, size, &[0, 40]), u32::MAX);
    let register = machine
        .resolve_import(DLL, "D3D11CoreRegisterLayers")
        .unwrap();
    assert_eq!(guest_call(&mut machine, register, &[0, 0]), 0);
    assert_eq!(guest_call(&mut machine, register, &[0, 1]), 0x80070057);
}

// spec: GFX-DXGI-1
#[test]
fn factory_luid_lookup_preserves_adapter_class() {
    let mut machine = machine();
    let (device, _) = create(&mut machine, 2, 0, &[0xb000]);
    let dxgi = query_face(&mut machine, device, IID_IDXGIDEVICE);
    assert_eq!(method(&mut machine, dxgi, 7, &[OUT]), 0);
    let adapter = machine.cpu.read_u32(OUT).unwrap();
    machine.cpu.write(IID, &IID_IDXGIFACTORY7).unwrap();
    assert_eq!(method(&mut machine, adapter, 6, &[IID, OUT]), 0);
    let factory = machine.cpu.read_u32(OUT).unwrap();
    machine.cpu.write(IID, &IID_IDXGIADAPTER1).unwrap();
    assert_eq!(method(&mut machine, factory, 26, &[2, 0, IID, OUT]), 0);
    let looked_up = machine.cpu.read_u32(OUT).unwrap();
    let desc = DATA + 0x750;
    assert_eq!(method(&mut machine, looked_up, 8, &[desc]), 0);
    let desc: adv_abi::types::DXGI_ADAPTER_DESC = GuestPtr::new(desc).read(&machine.cpu).unwrap();
    assert_eq!((desc.VendorId, desc.DeviceId), (0x1414, 0x008c));
    assert_eq!(
        method(&mut machine, factory, 26, &[2, 1, IID, OUT]),
        0x887a0002
    );
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
}

// spec: GFX-INTEROP-1
#[test]
fn registered_layers_receive_a_copy_with_advanced_argument_arrays() {
    let mut machine = machine();
    let register = machine
        .resolve_import(DLL, "D3D11CoreRegisterLayers")
        .unwrap();
    let size = machine
        .resolve_import(DLL, "D3D11CoreGetLayeredDeviceSize")
        .unwrap();
    let create = machine
        .resolve_import(DLL, "D3D11CoreCreateLayeredDevice")
        .unwrap();
    let get_size = machine.resolve_import("fixture", "layer_size").unwrap();
    let create_layer = machine.resolve_import("fixture", "layer_create").unwrap();
    let descriptors = DATA + 0x800;
    let args = DATA + 0x880;
    let firsts = DATA + 0x900;
    let indices = DATA + 0x910;
    let block = [0xc0de0000u32, 0xc0de0001, firsts, indices, 4, 5, 6, 7, 8, 9];
    machine
        .cpu
        .write(
            descriptors,
            bytemuck::bytes_of(&[11u32, 22, get_size, create_layer]),
        )
        .unwrap();
    machine.cpu.write(args, bytemuck::bytes_of(&block)).unwrap();
    machine.cpu.write_u32(firsts, 0xaaaa0001).unwrap();
    machine.cpu.write_u32(indices, 0).unwrap();
    assert_eq!(guest_call(&mut machine, register, &[descriptors, 1]), 0);
    assert_eq!(guest_call(&mut machine, register, &[descriptors, 1]), 0);
    assert_eq!(guest_call(&mut machine, size, &[args, 40]), 0x1234);
    assert_eq!(
        guest_call(&mut machine, create, &[args, 40, 0x77, IID, OUT]),
        0x123
    );
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0x5678);
    assert_eq!(
        GuestPtr::<[u32; 10]>::new(args).read(&machine.cpu).unwrap(),
        block
    );
    machine.cpu.write_u32(indices, 1).unwrap();
    assert_eq!(guest_call(&mut machine, size, &[args, 40]), u32::MAX);
}
