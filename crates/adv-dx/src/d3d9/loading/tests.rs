//! Public guest D3DX calls followed by real IDirect3DTexture9 vtable operations.
use super::*;
use adv_abi::{Conv, types::D3DLOCKED_RECT};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::RunResult;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x660000;
const OUTPUT: u32 = DATA + 0x100;
const INFO: u32 = DATA + 0x120;
const LOCK: u32 = DATA + 0x140;
const RESULT: u32 = DATA + 0x200;
const SOURCE: u32 = DATA + 0x400;
const READ_ONLY: u32 = DATA + 0x2000;

fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn call(code: &mut Asm, target: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target);
}
fn method(code: &mut Asm, slot: u8, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&OUTPUT.to_le_bytes());
    code.mov_eax_mem(OUTPUT).mov_eax_ptr_eax();
    code.bytes
        .extend_from_slice(&[0x8b, 0x40, slot * 4, 0xff, 0xd0]);
}
fn bmp() -> Vec<u8> {
    let mut bytes = vec![0; 66];
    bytes[..2].copy_from_slice(b"BM");
    for (at, value) in [
        (2, 66),
        (10, 54),
        (14, 40),
        (18, 3),
        (22, 1),
        (26, 0x180001),
        (34, 12),
    ] {
        bytes[at..at + 4].copy_from_slice(&u32::to_le_bytes(value));
    }
    bytes[54..63].copy_from_slice(&[120, 80, 40].repeat(3));
    bytes
}

#[test]
// spec: DEV-IMAGE-1
fn guest_image_front_preserves_pixels_typed_abi_guards_and_shared_com_heap_lifetime() {
    let mut m = super::super::tests::machine();
    m.sys.objects = Objects::default();
    m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
    let registry = m.sys.thunks.resolver_mut::<Registry>().unwrap();
    super::super::register(registry);
    crate::d3dx9::register(registry);
    for (name, argc) in [
        ("D3DXCreateTextureFromFileInMemoryEx", 15),
        ("D3DXGetImageInfoFromFileInMemory", 3),
    ] {
        assert_eq!(
            registry.get("d3dx9_43.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
    }
    registry.add("fixture.dll", "stop", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let table = alloc(&mut env(&mut m), 22 * 4).unwrap();
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "IDirect3DTexture9")
    {
        let thunk = m
            .resolve_import(
                "d3d9.dll",
                &format!("IDirect3DTexture9::{}", signature.name),
            )
            .unwrap();
        m.cpu
            .write_u32(table + u32::from(signature.com_slot.unwrap()) * 4, thunk)
            .unwrap();
    }
    m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", table);
    let (device, _, _, _) = super::super::tests::setup(&mut env(&mut m));
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(READ_ONLY, &[0xcc; 32]).unwrap();
    let bytes = bmp();
    m.cpu.write(SOURCE, &bytes).unwrap();
    let create = m
        .resolve_import("d3dx9_43.dll", "D3DXCreateTextureFromFileInMemoryEx")
        .unwrap();
    let info = m
        .resolve_import("d3dx9_43.dll", "D3DXGetImageInfoFromFileInMemory")
        .unwrap();
    let stop = m.resolve_import("fixture.dll", "stop").unwrap();
    let args = |out, metadata| {
        [
            device,
            SOURCE,
            bytes.len() as u32,
            u32::MAX - 1,
            u32::MAX - 1,
            1,
            0,
            21,
            2,
            3,
            3,
            0,
            metadata,
            0,
            out,
        ]
    };
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(RESULT + 80);
    call(&mut code, info, &[SOURCE, bytes.len() as u32, 0]);
    code.mov_mem_eax(RESULT);
    call(&mut code, info, &[SOURCE, bytes.len() as u32, READ_ONLY]);
    code.mov_mem_eax(RESULT + 4);
    call(&mut code, create, &args(READ_ONLY, INFO));
    code.mov_mem_eax(RESULT + 8);
    call(&mut code, create, &args(OUTPUT, READ_ONLY));
    code.mov_mem_eax(RESULT + 12);
    let mut unsupported = args(OUTPUT, INFO);
    unsupported[9] = 4;
    call(&mut code, create, &unsupported);
    code.mov_mem_eax(RESULT + 40);
    call(&mut code, create, &args(OUTPUT, INFO));
    code.mov_mem_eax(RESULT + 16);
    method(&mut code, 19, &[0, LOCK, 0, 0x10]);
    code.mov_mem_eax(RESULT + 20);
    code.mov_eax_mem(LOCK + 4)
        .mov_eax_ptr_eax()
        .mov_mem_eax(RESULT + 24);
    method(&mut code, 20, &[0]);
    code.mov_mem_eax(RESULT + 28);
    method(&mut code, 10, &[]);
    code.mov_mem_eax(RESULT + 32);
    method(&mut code, 2, &[]);
    code.mov_mem_eax(RESULT + 36);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(RESULT + 84);
    call(&mut code, stop, &[]);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("authored D3DX texture", CODE, &[], 65536).unwrap();
    let initial = m.sys.objects.object_count();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (offset, expected) in [
        (0, 0),
        (4, INVALID),
        (8, INVALID),
        (12, INVALID),
        (16, 0),
        (20, 0),
        (24, 0xff285078),
        (28, 0),
        (32, 3),
        (36, 0),
        (40, UNAVAILABLE),
    ] {
        assert_eq!(
            m.cpu.read_u32(RESULT + offset).unwrap(),
            expected,
            "result +{offset}"
        );
    }
    assert_eq!(
        m.cpu.read_u32(RESULT + 80).unwrap(),
        m.cpu.read_u32(RESULT + 84).unwrap()
    );
    assert_eq!(
        GuestPtr::<[u32; 7]>::new(INFO).read(&m.cpu).unwrap(),
        [3, 1, 1, 1, 20, 3, 0]
    );
    let pointer = m.cpu.read_u32(OUTPUT).unwrap();
    let storage = GuestPtr::<D3DLOCKED_RECT>::new(LOCK)
        .read(&m.cpu)
        .unwrap()
        .pBits;
    let heap = m.ext.get::<Heaps>().process_heap;
    assert_eq!(m.ext.get_mut::<Heaps>().size(heap, pointer), None);
    assert_eq!(m.ext.get_mut::<Heaps>().size(heap, storage), None);
    assert_eq!(m.sys.objects.object_count(), initial);
    assert!(m.ext.get::<Loading>().pending.is_empty());
    assert_eq!(m.cpu.read_u32(READ_ONLY).unwrap(), 0xcccccccc);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(
                |(api, reason)| api == "d3dx9_43.dll!D3DXCreateTextureFromFileInMemoryEx"
                    && reason.contains("filter")
            )
    );

    // The common cancellation ticket rejects a late successful callback after
    // process cancellation; no dangling pointer or duplicate Release is exposed.
    let guard = Rc::new(());
    assert!(matches!(
        finish(&mut env(&mut m), pointer, guard, Outcome::Return(0)),
        Outcome::Return(INVALID)
    ));

    // Managed textures use the same initial upload and the same shared GPU
    // surface owner as guest-created textures. Read back pixels before Release.
    #[derive(Default)]
    struct Images {
        next: u32,
        targets: BTreeMap<u32, Target>,
    }
    m.ext.insert(Images::default());
    let state = m.ext.get_mut::<State>();
    state.gpu = Some(pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap());
    state.provider.insert = |ext, target| {
        let images = ext.get_mut::<Images>();
        images.next += 1;
        images.targets.insert(images.next, target);
        Ok(images.next)
    };
    state.provider.target = |ext, id| ext.get::<Images>().targets.get(&id).cloned();
    state.provider.remove = |ext, _, id| {
        ext.get_mut::<Images>().targets.remove(&id).unwrap();
        Ok(())
    };
    let expected = [120, 80, 40, 160].repeat(3);
    let outcome = super::create(&mut env(&mut m), device, 3, 1, 21, 1, expected.clone());
    let Outcome::Return(pointer) = outcome else {
        panic!("tiny immediate texture upload unexpectedly blocked");
    };
    assert!(pointer != 0 && pointer & 0x80000000 == 0);
    let surface = resources::resource(&env(&mut m), pointer)
        .unwrap()
        .surface
        .unwrap();
    let image = m.ext.get::<Images>().targets[&surface].image.clone();
    let mut transfer = m
        .ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .readback(&image, None)
        .unwrap();
    transfer.take_completion_job().unwrap()();
    assert_eq!(transfer.poll().unwrap().unwrap(), expected);
    assert_eq!(release(&mut env(&mut m), pointer), 0);
    assert!(m.ext.get::<Images>().targets.is_empty());
    assert_eq!(m.sys.objects.object_count(), initial);
}
