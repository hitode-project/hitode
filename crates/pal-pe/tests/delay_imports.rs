mod support {
    pub mod delay_fixture;
}
use pal_pe::{ImportName, Pe, PeError};
use support::delay_fixture::{BASE, DESC, HMOD, IAT, INT, image};

fn check(pe: &Pe, base: u32) {
    let modules = pe.delay_imports_at(base).unwrap();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.dll, "kernel32.dll");
    assert_eq!(module.module_handle_rva, HMOD + 0x300);
    assert_eq!(module.imports.len(), 2);
    assert_eq!(module.imports[0].iat_rva, IAT + 0x300);
    assert_eq!(module.imports[0].name, ImportName::Name { hint: 42, name: "FixtureCall".into() });
    assert_eq!(module.imports[1].iat_rva, IAT + 0x304);
    assert_eq!(module.imports[1].name, ImportName::Ordinal(7));
}

#[test]
fn old_va_and_current_rva_delay_imports_decode_the_same_names_and_ordinals() {
    for rva_based in [false, true] {
        let pe = Pe::parse(&image(rva_based)).unwrap();
        check(&pe, BASE);
        assert_eq!(
            pe.delay_imports().unwrap()[0].imports[0].name,
            ImportName::Name { hint: 42, name: "FixtureCall".into() }
        );
    }
}

#[test]
fn rebased_old_va_names_use_the_loaded_base_and_preserve_ordinal_bits() {
    let mut pe = Pe::parse(&image(false)).unwrap();
    let base = 0x1040_0000;
    assert_eq!(pe.relocate(base - BASE).unwrap(), 5);
    check(&pe, base);
    assert!(pe.delay_imports().is_err(), "unrelocated base must not accept rebased VAs");
    let current = Pe::parse(&image(true)).unwrap();
    check(&current, base);
}

#[test]
fn malformed_old_pointer_and_reserved_attributes_remain_errors() {
    let mut pe = Pe::parse(&image(false)).unwrap();
    pe.set_u32(INT + 0x300, BASE - 1).unwrap();
    assert!(matches!(pe.delay_imports(), Err(PeError::Malformed(_))));
    pe.set_u32(INT + 0x300, BASE + pe.size_of_image).unwrap();
    assert!(matches!(pe.delay_imports(), Err(PeError::Malformed(_))));
    pe.set_u32(DESC + 0x300, 2).unwrap();
    assert!(matches!(pe.delay_imports(), Err(PeError::Malformed(_))));
}
