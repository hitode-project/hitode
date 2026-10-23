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
