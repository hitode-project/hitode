use adv_pe::{
    Export, ExportTarget, Import, ImportModule, ImportName, Pe, dir, scn, write::DllWriter,
};

#[test]
fn dll_round_trips_sections_exports_imports_and_relocations() {
    let preferred = 0x1000_0000;
    let mut writer = DllWriter::new("roundtrip.dll", preferred);
    let text = writer
        .section(
            ".text",
            vec![0xc3],
            0x1000,
            scn::CNT_CODE | scn::MEM_READ | scn::MEM_EXECUTE,
        )
        .unwrap();
    writer.set_entry(text);
    let mut data = vec![0; 0x1004];
    data[..4].copy_from_slice(&(preferred + text).to_le_bytes());
    data[0x1000..].copy_from_slice(&(preferred + text + 1).to_le_bytes());
    let data_rva = writer
        .section(".data", data, 0x2000, scn::MEM_READ | scn::MEM_WRITE | 0x40)
        .unwrap();
    writer
        .exports(&[
            Export {
                name: Some("Zed".into()),
                ordinal: 9,
                target: ExportTarget::Rva(text),
            },
            Export {
                name: Some("Alpha".into()),
                ordinal: 3,
                target: ExportTarget::Rva(text),
            },
            Export {
                name: None,
                ordinal: 5,
                target: ExportTarget::Rva(data_rva),
            },
        ])
        .unwrap();
    let assigned = writer
        .imports(&[
            ImportModule {
                dll: "kernel32.dll".into(),
                imports: vec![Import {
                    name: ImportName::Name {
                        hint: 7,
                        name: "GetCurrentProcessId".into(),
                    },
                    iat_rva: 0,
                }],
            },
            ImportModule {
                dll: "other.dll".into(),
                imports: vec![Import {
                    name: ImportName::Ordinal(17),
                    iat_rva: 0,
                }],
            },
        ])
        .unwrap();
    writer.relocations(&[data_rva + 0x1000, data_rva]).unwrap();
    let bytes = writer.finish().unwrap();
    let mut pe = Pe::parse(&bytes).unwrap();
    assert!(pe.is_dll());
    assert_eq!(pe.entry_rva, text);
    assert_eq!(pe.sections[1].virtual_size, 0x2000);
    assert_eq!(pe.u32_at(data_rva + 0x1004).unwrap(), 0);
    let exports = pe.exports().unwrap();
    assert_eq!(
        exports
            .iter()
            .map(|e| (e.ordinal, e.name.as_deref()))
            .collect::<Vec<_>>(),
        [(3, Some("Alpha")), (5, None), (9, Some("Zed"))]
    );
    assert_eq!(exports[0].target, ExportTarget::Rva(text));
    let imports = pe.imports().unwrap();
    assert_eq!(imports.len(), 2);
    for (actual, expected) in imports.iter().zip(&assigned) {
        assert_eq!(actual.dll, expected.dll);
        assert_eq!(actual.imports[0].name, expected.imports[0].name);
        assert_eq!(actual.imports[0].iat_rva, expected.imports[0].iat_rva);
    }
    assert!(pe.dir(dir::IAT).is_present());
    assert_eq!(pe.relocations().unwrap(), [data_rva, data_rva + 0x1000]);
    assert_eq!(pe.rebase(0x3000_0000).unwrap(), 2);
    assert_eq!(pe.u32_at(data_rva).unwrap(), 0x3000_0000 + text);
    assert_eq!(
        pe.u32_at(data_rva + 0x1000).unwrap(),
        0x3000_0000 + text + 1
    );
    // The loader binary-searches export names, independently of ordinals.
    let directory = pe.dir(dir::EXPORT).rva;
    let names = pe.u32_at(directory + 32).unwrap();
    assert_eq!(pe.cstr_at(pe.u32_at(names).unwrap()).unwrap(), b"Alpha");
    assert_eq!(pe.cstr_at(pe.u32_at(names + 4).unwrap()).unwrap(), b"Zed");
}

// A one-leaf resource tree (type 16, name 1, language 0x409) at `rva`.
fn resource_section(rva: u32) -> Vec<u8> {
    let mut bytes = vec![0; 96];
    let directory = |bytes: &mut Vec<u8>, at: usize, id: u32, target: u32| {
        bytes[at + 14..at + 16].copy_from_slice(&1u16.to_le_bytes());
        bytes[at + 16..at + 20].copy_from_slice(&id.to_le_bytes());
        bytes[at + 20..at + 24].copy_from_slice(&target.to_le_bytes());
    };
    directory(&mut bytes, 0, 16, 0x8000_0000 | 24);
    directory(&mut bytes, 24, 1, 0x8000_0000 | 48);
    directory(&mut bytes, 48, 0x409, 72);
    bytes[72..76].copy_from_slice(&(rva + 88).to_le_bytes());
    bytes[76..80].copy_from_slice(&5u32.to_le_bytes());
    bytes[88..93].copy_from_slice(b"hello");
    bytes
}

// spec: WINE-DLL-4
#[test]
fn replacement_dll_carries_the_original_resources() {
    let mut original = DllWriter::new("original.dll", 0x1000_0000);
    let rva = original.next_rva();
    let section = resource_section(rva);
    let len = section.len() as u32;
    original
        .section(".rsrc", section, len, scn::MEM_READ | 0x40)
        .unwrap();
    let mut bytes = original.finish().unwrap();
    // The resource data directory of the PE32 optional header.
    bytes[0x108..0x10c].copy_from_slice(&rva.to_le_bytes());
    bytes[0x10c..0x110].copy_from_slice(&88u32.to_le_bytes());
    let original = Pe::parse(&bytes).unwrap();

    let mut writer = DllWriter::new("replacement.dll", 0x1000_0000);
    writer
        .section(".text", vec![0xc3], 0x2000, scn::CNT_CODE | scn::MEM_READ)
        .unwrap();
    writer.resources(&original).unwrap();
    let pe = Pe::parse(&writer.finish().unwrap()).unwrap();
    assert_ne!(pe.dir(dir::RESOURCE).rva, rva);
    let leaves = pe.resources().unwrap().unwrap().leaves();
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].lang, 0x409);
    let data = leaves[0].data;
    assert_eq!(pe.slice(data.rva, data.size).unwrap(), b"hello");
}
