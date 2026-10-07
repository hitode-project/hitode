//! Authored native PE groups 101/202 keep ordinal 0/1; ICO ignores its WORD.
//! See ignored associated-icon-native/native.jsonl for direct-source A/W cases.
use super::*;

fn store_esp(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}

#[test]
fn gpa_fronts_keep_direct_paths_ordinals_and_create_real_owned_images() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        f.machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let dll = f.path("shell32.dll", false);
        // LoadLibrary creates the process-owned loader mutex on its first call.
        // Establish that genuine owner before measuring icon/list lifetimes.
        let mut warm = Asm::new(CODE);
        let loaded = f.call(&mut warm, "kernel32.dll!LoadLibraryA", &[I(dll)]);
        f.run(warm);
        assert_ne!(f.u32(loaded), 0);
        let before = f.machine.sys.objects.object_count();
        let paths = [
            f.path("図像.dll", false),
            f.path("図像.dll", true),
            f.path("図像.ico", true),
        ];
        let indices = [
            f.bytes(&[0, 0, 0xcc, 0xcc]),
            f.bytes(&[1, 0, 0xdd, 0xdd]),
            f.bytes(&[1, 0, 0xee, 0xee]),
        ];
        let names = [
            f.path("ExtractAssociatedIconA", false),
            f.path("ExtractAssociatedIconW", false),
        ];
        let originals: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(i, &address)| {
                let length = if i == 0 { 9 } else { 14 }; // CP932 DLL / UTF16 DLL or ICO plus NUL.
                let mut bytes = vec![0; length];
                f.machine.cpu.read(address, &mut bytes).unwrap();
                bytes
            })
            .collect();
        let mut code = Asm::new(CODE);
        let module = f.call(&mut code, "kernel32.dll!LoadLibraryA", &[I(dll)]);
        let procedures = names.map(|name| {
            f.call(
                &mut code,
                "kernel32.dll!GetProcAddress",
                &[M(module), I(name)],
            )
        });
        let list = f.call(
            &mut code,
            "comctl32.dll!ImageList_Create",
            &[I(32), I(32), I(0x20), I(0), I(1)],
        );
        let mut results = Vec::new();
        for i in 0..3 {
            f.call(&mut code, "kernel32.dll!SetLastError", &[I(0x12345678)]);
            let result = f.bytes(&[0; 20]);
            store_esp(&mut code, result + 8);
            code.push_imm(indices[i]).push_imm(paths[i]).push_imm(CODE);
            code.mov_eax_mem(procedures[usize::from(i != 0)]);
            code.bytes.extend_from_slice(&[0xff, 0xd0]);
            code.mov_mem_eax(result);
            store_esp(&mut code, result + 12);
            let error = f.call(&mut code, "kernel32.dll!GetLastError", &[]);
            code.mov_eax_mem(error).mov_mem_eax(result + 4);
            // ImageList_ReplaceIcon accepts only an actual same-sized IconImage,
            // and copies it before DestroyIcon retires the original USER owner.
            let added = f.call(
                &mut code,
                "comctl32.dll!ImageList_ReplaceIcon",
                &[M(list), I(u32::MAX), M(result)],
            );
            code.mov_eax_mem(added).mov_mem_eax(result + 16);
            f.destroy(&mut code, result, 1);
            results.push(result);
        }
        let size_x = f.bytes(&[0; 4]);
        let size_y = f.bytes(&[0; 4]);
        let size = f.call(
            &mut code,
            "comctl32.dll!ImageList_GetIconSize",
            &[M(list), I(size_x), I(size_y)],
        );
        let destroyed = f.call(&mut code, "comctl32.dll!ImageList_Destroy", &[M(list)]);
        f.run(code);
        assert_ne!(f.u32(module), 0);
        for procedure in procedures {
            assert_ne!(f.u32(procedure), 0);
        }
        for (i, &result) in results.iter().enumerate() {
            assert_ne!(f.u32(result), 0);
            assert_eq!(f.u32(result + 4), 0x12345678);
            assert_eq!(f.u32(result + 8), f.u32(result + 12), "stdcall3 cleanup");
            assert_eq!(f.u32(result + 16), i as u32);
            let mut bytes = vec![0; originals[i].len()];
            f.machine.cpu.read(paths[i], &mut bytes).unwrap();
            assert_eq!(bytes, originals[i]);
        }
        assert_eq!(f.u32(indices[0]), 0xcccc0000);
        assert_eq!(f.u32(indices[1]), 0xdddd0001);
        assert_eq!(f.u32(indices[2]), 0xeeee0001);
        assert_eq!((f.u32(size_x), f.u32(size_y)), (32, 32));
        assert_eq!(f.u32(size), 1);
        assert_eq!(f.u32(destroyed), 1);
        let diagnostics = f.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(diagnostics.dynamic_lookup_failures.is_empty());
        assert!(diagnostics.warnings.is_empty());
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}

#[test]
fn invalid_inout_and_unavailable_associations_do_not_mutate_or_allocate() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        f.machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let before = f.machine.sys.objects.object_count();
        let pe_path = f.path("図像.dll", true);
        let pe_id = f.bytes(&[101, 0, 0x88, 0x88]);
        let index = f.bytes(&[0, 0, 0xaa, 0xaa]);
        let unsupported = f.path("unknown.bin", false);
        let missing = f.path("missing.ico", true);
        let path_bytes: Vec<_> = "図像.dll"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let readonly = CODE + 0xe00;
        f.machine.cpu.write(readonly, &path_bytes).unwrap();
        let short = DATA + 4094;
        f.machine
            .cpu
            .write(short, &('X' as u16).to_le_bytes())
            .unwrap();
        let mut code = Asm::new(CODE);
        let mut outputs = Vec::new();
        for (api, path, word, error) in [
            ("ExtractAssociatedIconW", pe_path, CODE + 0xf00, 998),
            ("ExtractAssociatedIconW", readonly, index, 998),
            ("ExtractAssociatedIconW", short, index, 998),
            ("ExtractAssociatedIconW", pe_path, u32::MAX, 998),
            ("ExtractAssociatedIconW", pe_path, pe_id, 50),
            ("ExtractAssociatedIconA", unsupported, index, 50),
            ("ExtractAssociatedIconW", missing, index, 2),
        ] {
            let result = f.call(&mut code, api, &[I(CODE), I(path), I(word)]);
            let last_error = f.call(&mut code, "kernel32.dll!GetLastError", &[]);
            outputs.push((result, last_error, error));
        }
        f.run(code);
        for (result, last_error, error) in outputs {
            assert_eq!(f.u32(result), 0);
            assert_eq!(f.u32(last_error), error);
        }
        assert_eq!(f.u32(pe_id), 0x88880065); // Resource ID is not the ordinal.
        assert_eq!(f.u32(index), 0xaaaa0000);
        let mut after = vec![0; path_bytes.len()];
        f.machine.cpu.read(pe_path, &mut after).unwrap();
        assert_eq!(after, path_bytes);
        f.machine.cpu.read(readonly, &mut after).unwrap();
        assert_eq!(after, path_bytes);
        assert_eq!(f.machine.cpu.read_u16(short).unwrap(), 'X' as u16);
        let diagnostics = f.machine.ext.get::<adv_abi::ApiDiagnostics>();
        for api in [
            "shell32.dll!ExtractAssociatedIconW",
            "shell32.dll!ExtractAssociatedIconA",
        ] {
            assert!(
                diagnostics
                    .warnings
                    .keys()
                    .any(|(name, reason)| name == api && reason.contains("associations"))
            );
        }
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}

#[test]
fn associated_and_existing_extractors_read_dirty_shared_file_bytes_without_flushing() {
    const VIEW: u32 = 0x500000;
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        f.machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let host = f.machine.ext.get::<Win32>().vfs.lookup("図像.ico").unwrap();
        let backing = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&host)
            .unwrap();
        let len = backing.metadata().unwrap().len();
        let section = f
            .machine
            .ext
            .get::<Win32>()
            .vfs
            .sections
            .file(backing, len, true)
            .unwrap();
        let page = section.page(0);
        f.machine
            .cpu
            .map_file_pages(VIEW, &[page.clone()], Prot::RW, false)
            .unwrap();
        let before = f.machine.sys.objects.object_count();
        let path = f.path("図像.ico", true);
        let index = f.bytes(&[1, 0, 0xcc, 0xcc]);
        // The real mapped page differs from disk before any flush; an ordinary
        // file read would incorrectly create an icon from the disk's ICO header.
        f.machine.cpu.write_u32(VIEW, 0).unwrap();
        assert_eq!(&std::fs::read(&host).unwrap()[..4], &[0, 0, 1, 0]);
        let mut code = Asm::new(CODE);
        let dirty = f.call(
            &mut code,
            "ExtractAssociatedIconW",
            &[I(CODE), I(path), I(index)],
        );
        let error = f.call(&mut code, "kernel32.dll!GetLastError", &[]);
        let old_dirty = f.call(
            &mut code,
            "ExtractIconExW",
            &[I(path), I(u32::MAX), I(0), I(0), I(0)],
        );
        code.mov_mem_imm(VIEW, 0x00010000);
        let restored = f.call(
            &mut code,
            "ExtractAssociatedIconW",
            &[I(CODE), I(path), I(index)],
        );
        let old_restored = f.call(
            &mut code,
            "ExtractIconExW",
            &[I(path), I(u32::MAX), I(0), I(0), I(0)],
        );
        f.destroy(&mut code, restored, 1);
        f.run(code);
        assert_eq!(f.u32(dirty), 0);
        assert_eq!(f.u32(error), 50);
        assert_eq!(f.u32(old_dirty), 0);
        assert_ne!(f.u32(restored), 0);
        assert_eq!(f.u32(old_restored), 1);
        assert_eq!(f.u32(index), 0xcccc0001);
        assert_eq!(
            f.machine.sys.objects.hosts::<adv_win32::FileObj>().count(),
            0
        );
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
        f.machine.cpu.unmap(VIEW, 4096).unwrap();
    }
}
