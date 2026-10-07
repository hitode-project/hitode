//! Object queries preserve logical creation data and share one GDI namespace.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{
    Clock, Machine,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};

const DATA: u32 = 0x410000;
fn fixture() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.dynamic_available("gdi32.dll", "GetObjectW"));
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/object-query-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    machine
        .spawn("object queries", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(DATA + 4096, 4096, Prot::READ).unwrap();
    machine
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn call(machine: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> u32 {
    machine.cpu.set_reg(Reg::Esp, DATA);
    for (index, arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *arg)
            .unwrap();
    }
    let address = machine.resolve_import("gdi32.dll", name).unwrap();
    let handler = machine
        .sys
        .thunks
        .get((address - THUNK_BASE) / THUNK_STRIDE)
        .f
        .handler;
    let Outcome::Return(value) = handler(&mut env(machine)) else {
        panic!("immediate object query expected")
    };
    value
}
fn bytes(machine: &Machine<JitCpu>, address: u32, size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    machine.cpu.read(address, &mut bytes).unwrap();
    bytes
}

#[test]
fn wide_font_description_retains_unicode_and_checked_byte_boundaries() {
    let mut machine = fixture();
    let before = machine.sys.objects.object_count();
    let mut logical = [0; 92];
    logical[..4].copy_from_slice(&(-19i32).to_le_bytes());
    logical[16..20].copy_from_slice(&500i32.to_le_bytes());
    logical[23] = 1;
    for (slot, unit) in logical[28..]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip("字體😀".encode_utf16())
    {
        *slot = unit.to_le_bytes();
    }
    logical[40..42].copy_from_slice(&0x4e01u16.to_le_bytes());
    machine.cpu.write(DATA + 128, &logical).unwrap();
    let font = call(&mut machine, "CreateFontIndirectW", &[DATA + 128]);
    assert_ne!(font, 0);
    machine.cpu.write(DATA + 128, &[0; 92]).unwrap();
    crate::util::set_last_error(&mut env(&mut machine), 0x12345678);
    assert_eq!(call(&mut machine, "GetObjectW", &[font, 0, 0]), 92);
    for count in [1, 91, 92] {
        machine.cpu.write(DATA + 512, &[0xcc; 96]).unwrap();
        assert_eq!(
            call(&mut machine, "GetObjectW", &[font, count, DATA + 512]),
            count
        );
        assert_eq!(
            bytes(&machine, DATA + 512, count as usize),
            logical[..count as usize]
        );
        assert_eq!(
            bytes(&machine, DATA + 512 + count, 96 - count as usize),
            vec![0xcc; 96 - count as usize]
        );
    }
    assert_eq!(
        call(&mut machine, "GetObjectA", &[font, 60, DATA + 768]),
        60
    );
    assert_eq!(bytes(&machine, DATA + 768, 28), logical[..28]);
    machine.cpu.write(DATA + 4096 - 91, &[0xcc; 91]).unwrap();
    assert_eq!(
        call(&mut machine, "GetObjectW", &[font, 92, DATA + 4096 - 91]),
        0
    );
    assert_eq!(bytes(&machine, DATA + 4096 - 91, 91), vec![0xcc; 91]);
    assert_eq!(
        call(&mut machine, "GetObjectW", &[font, 92, DATA + 4096]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 0x12345678);
    assert_eq!(call(&mut machine, "DeleteObject", &[font]), 1);
    assert_eq!(call(&mut machine, "GetObjectW", &[font, 0, 0]), 0);
    assert_eq!(machine.sys.objects.object_count(), before);
}

#[cfg(windows)]
#[test]
fn stock_font_and_nonfont_descriptions_share_a_w_ownership() {
    let mut machine = fixture();
    let before = machine.sys.objects.object_count();
    let stock = call(&mut machine, "GetStockObject", &[17]);
    assert_eq!(stock, stock_object(17));
    crate::util::set_last_error(&mut env(&mut machine), 0x12345678);
    assert_eq!(call(&mut machine, "GetObjectW", &[stock, 0, 0]), 92);
    assert_eq!(call(&mut machine, "GetObjectA", &[stock, 0, 0]), 60);
    assert_eq!(
        call(&mut machine, "GetObjectW", &[stock, 92, DATA + 512]),
        92
    );
    assert_eq!(
        call(&mut machine, "GetObjectA", &[stock, 60, DATA + 768]),
        60
    );
    assert_eq!(
        bytes(&machine, DATA + 512, 28),
        bytes(&machine, DATA + 768, 28)
    );
    assert!(
        bytes(&machine, DATA + 540, 64)
            .iter()
            .any(|byte| *byte != 0)
    );
    assert_eq!(machine.sys.objects.object_count(), before);
    let pen = call(&mut machine, "CreatePen", &[0, 2, 0x123456]);
    assert_eq!(call(&mut machine, "GetObjectW", &[pen, 15, DATA + 512]), 0);
    assert_eq!(call(&mut machine, "GetObjectW", &[pen, 16, DATA + 512]), 16);
    assert_eq!(call(&mut machine, "GetObjectA", &[pen, 16, DATA + 768]), 16);
    assert_eq!(
        bytes(&machine, DATA + 512, 16),
        bytes(&machine, DATA + 768, 16)
    );
    assert_eq!(call(&mut machine, "DeleteObject", &[pen]), 1);
    assert_eq!(call(&mut machine, "GetObjectW", &[0, 92, DATA + 512]), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 0x12345678);
    assert_eq!(machine.sys.objects.object_count(), before);
}
