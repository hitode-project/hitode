//! user32's own window creation bypasses its hookable exports.
use adv_cpu::Cpu;
use adv_cpu::backend::Guest;
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::Pe;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

#[test]
fn dialogs_create_windows_past_the_exports() {
    let Some(dist) = wine::dist() else {
        return;
    };
    let dll = std::path::Path::new(&dist).join("lib/wine/i386-windows/user32.dll");
    let image = std::fs::read(&dll).unwrap();
    let pe = Pe::parse(&image).unwrap();
    let mut machine = Machine::new(
        Cpu::new(PARK_ADDR, false).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    adv_nt::image::map(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        &pe,
        pe.image_base,
    )
    .unwrap();
    adv_nt::identity::note_ntdll(&mut machine, &dll, &image);
    let table = machine.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let base = pe.image_base;
    let read = |at: u32, len: u32| {
        let mut bytes = vec![0; len as usize];
        machine.cpu.read(at, &mut bytes).unwrap();
        bytes
    };
    let exports =
        ["CreateWindowExA", "CreateWindowExW"].map(|name| base + table.named(name).unwrap().rva);
    let dialog = table.named("DIALOG_CreateIndirect").unwrap();
    let code = read(base + dialog.rva, dialog.size);
    let mut decoder = Decoder::with_ip(
        32,
        &code,
        u64::from(base + dialog.rva),
        DecoderOptions::NONE,
    );
    let mut private = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.mnemonic() == Mnemonic::Call
            && instruction.op0_kind() == OpKind::NearBranch32
        {
            let target = instruction.near_branch32();
            assert!(
                !exports.contains(&target),
                "{:#x} calls an export",
                instruction.ip32()
            );
            if !(base..base + pe.size_of_image).contains(&target) {
                private.push(target);
            }
        }
    }
    // The dialog and its controls, through the ANSI and Unicode entries.
    assert_eq!(private.len(), 4);
    for target in private {
        // The export's prologue, then a jump past it.
        let code = read(target, 10);
        assert_eq!(code[5], 0xe9);
        let resume =
            (target + 10).wrapping_add(u32::from_le_bytes(code[6..10].try_into().unwrap()));
        assert!(exports.contains(&(resume - 5)));
        assert_eq!(code[..5], read(resume - 5, 5));
    }
}
