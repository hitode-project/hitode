use super::tests::{ansi, bytes, error, fixture, profile_call, utf16, wide};
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg, asm::Asm},
};
use adv_machine::{RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;
fn complete<B: Backend>(cpu: B) {
    let (mut machine, root) = fixture(cpu, "complete-family");
    machine
        .spawn("profile calls", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    for (unicode, suffix) in [(false, "A"), (true, "W")] {
        let contents = [
            &[0xff, 0xfe],
            utf16("[S]\r\nold=remove\r\n; authored comment\r\n[Keep]\r\nunrelated=present\r\n")
                .as_slice(),
        ]
        .concat();
        std::fs::write(root.join("game/expanded.ini"), contents).unwrap();
        // VFS names are indexed at first lookup, so remove the prior overlay
        // through VFS mutation instead of replacing host files under its cache.
        if let Some(path) = machine.ext.get::<Win32>().vfs.lookup(".\\expanded.ini")
            && path.starts_with(root.join("data"))
        {
            let path = machine
                .ext
                .get::<Win32>()
                .vfs
                .write_path(".\\expanded.ini")
                .unwrap();
            std::fs::write(
                path,
                [
                    &[0xff, 0xfe],
                    utf16("[S]\r\nold=remove\r\n[Keep]\r\nunrelated=present\r\n").as_slice(),
                ]
                .concat(),
            )
            .unwrap();
        }
        for (offset, text) in [
            (0x400, "S"),
            (0x500, "blob"),
            (0x600, ".\\expanded.ini"),
            (0x700, "更新"),
            (0x800, "a=1\0b=two\0"),
            (0x900, "bad"),
            (0xa00, "0080FF7F00"),
        ] {
            if unicode {
                wide(&mut machine, DATA + offset, text);
            } else {
                let value = machine
                    .ext
                    .get::<Win32>()
                    .codec
                    .from_utf16(0, &text.encode_utf16().collect::<Vec<_>>())
                    .0;
                ansi(&mut machine, DATA + offset, &value);
            }
        }
        let name = |base: &str| format!("{base}{suffix}");
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileSection"),
                &[DATA + 0x400, DATA + 0x800, DATA + 0x600]
            ),
            1
        );
        machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileSection"),
                &[DATA + 0x400, OUT, 64, DATA + 0x600]
            ),
            10
        );
        let expected = if unicode {
            utf16("a=1\0b=two\0\0")
        } else {
            b"a=1\0b=two\0\0".to_vec()
        };
        assert_eq!(bytes(&machine, OUT, expected.len()), expected);
        assert_eq!(error(&mut machine), 0);
        machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileSection"),
                &[DATA + 0x400, OUT, 6, DATA + 0x600]
            ),
            4
        );
        assert_eq!(error(&mut machine), 234);
        let expected = if unicode {
            utf16("a=1\0\0\0")
        } else {
            b"a=1\0\0\0".to_vec()
        };
        assert_eq!(bytes(&machine, OUT, expected.len()), expected);
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileString"),
                &[DATA + 0x400, DATA + 0x500, DATA + 0x700, DATA + 0x600]
            ),
            1
        );
        let saved = machine
            .ext
            .get::<Win32>()
            .vfs
            .lookup(".\\expanded.ini")
            .unwrap();
        let saved = std::fs::read(saved).unwrap();
        assert!(saved.starts_with(&[0xff, 0xfe]));
        let text = String::from_utf16(
            &saved[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.contains("blob=更新\r\n"));
        assert!(text.contains("[Keep]\r\nunrelated=present\r\n"));
        assert!(!text.contains("old=remove"));
        machine
            .cpu
            .write(DATA + 0xb00, &[0, 128, 255, 127])
            .unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, DATA + 0xb00, 4, DATA + 0x600]
            ),
            1
        );
        machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, OUT, 4, DATA + 0x600]
            ),
            1
        );
        assert_eq!(bytes(&machine, OUT, 6), [0, 128, 255, 127, 0xcc, 0xcc]);
        machine.cpu.protect(OUT + 4096, 4096, Prot::READ).unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, OUT + 4096, 4, DATA + 0x600]
            ),
            0
        );
        assert_eq!(error(&mut machine), 998);
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileString"),
                &[DATA + 0x400, DATA + 0x500, DATA + 0xa00, DATA + 0x600]
            ),
            1
        );
        machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, OUT, 4, DATA + 0x600]
            ),
            0
        );
        assert_eq!(error(&mut machine), 13);
        assert_eq!(bytes(&machine, OUT, 64), [0xcc; 64]);
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, 0, 0, DATA + 0x600]
            ),
            1
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileStruct"),
                &[DATA + 0x400, DATA + 0x500, OUT, 4, DATA + 0x600]
            ),
            0
        );
        assert_eq!(error(&mut machine), 2);
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WriteProfileSection"),
                &[DATA + 0x400, DATA + 0x800]
            ),
            1
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetProfileSection"),
                &[DATA + 0x400, OUT, 64]
            ),
            10
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WriteProfileString"),
                &[DATA + 0x400, DATA + 0x500, DATA + 0x700]
            ),
            1
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WriteProfileSection"),
                &[DATA + 0x400, 0]
            ),
            1
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetProfileSection"),
                &[DATA + 0x400, OUT, 64]
            ),
            0
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileString"),
                &[0, 0, 0, DATA + 0x600]
            ),
            0
        );
        assert_eq!(error(&mut machine), 0);
        assert_eq!(
            profile_call(
                &mut machine,
                &name("WritePrivateProfileSection"),
                &[DATA + 0x400, 0, DATA + 0x600]
            ),
            1
        );
        assert_eq!(
            profile_call(
                &mut machine,
                &name("GetPrivateProfileSection"),
                &[DATA + 0x400, OUT, 64, DATA + 0x600]
            ),
            0
        );
    }
    let diagnostics = machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}
fn native_failure<B: Backend>(cpu: B) {
    let (mut machine, root) = fixture(cpu, "native-writer");
    std::fs::write(root.join("game/write.ini"), b"[S]\r\nk=original\r\n").unwrap();
    for (offset, value) in [
        (0x400, b"S".as_slice()),
        (0x500, b"k"),
        (0x600, b"replacement"),
        (0x700, b".\\write.ini"),
    ] {
        ansi(&mut machine, DATA + offset, value);
    }
    let entry = {
        let (win32, modules) = machine.ext.get2_mut::<Win32, adv_os::Modules>();
        let mut loader =
            adv_os::Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs);
        let module = loader.load("ntdll.dll").unwrap();
        loader
            .resolve_dynamic(
                module,
                &adv_pe::ImportName::Name {
                    hint: 0,
                    name: "NtWriteFile".into(),
                },
            )
            .unwrap()
            .0
    };
    let mut failure = Asm::new(entry);
    failure
        .mov_mem_imm(DATA + 0x800, 0xc0000022)
        .mov_eax_mem(DATA + 0x800)
        .ret_n(36);
    machine.cpu.write(entry, &failure.bytes).unwrap();
    let write = machine
        .resolve_import(K32, "WritePrivateProfileStringA")
        .unwrap();
    let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA + 0x700)
        .push_imm(DATA + 0x600)
        .push_imm(DATA + 0x500)
        .push_imm(DATA + 0x400)
        .call(write)
        .mov_mem_eax(DATA)
        .call(get_error)
        .mov_mem_eax(DATA + 4)
        .mov_mem_imm(DATA + 8, 0)
        .mov_eax_mem(DATA + 8)
        .ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("native profile writer", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 5);
    let path = machine
        .ext
        .get::<Win32>()
        .vfs
        .lookup(".\\write.ini")
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"[S]\r\nk=original\r\n");
    assert_eq!(machine.sys.objects.all_hosts::<FileObj>().count(), 0);
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
// spec: PERS-PROFILEFILE-1, PERS-PROFILEFILE-2
fn whole_sections_structures_and_unicode_writes_share_profile_files_on_all_backends() {
    for jit in [false, true] {
        complete(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    complete(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
// spec: PERS-PROFILEFILE-3
fn modified_native_writes_control_profile_failures_without_truncating_files() {
    for jit in [false, true] {
        native_failure(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    native_failure(UnicornCpu::new(PARK_ADDR).unwrap());
}
