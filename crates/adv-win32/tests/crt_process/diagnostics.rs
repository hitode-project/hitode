use super::*;

// spec: CRT-LIB-15
#[test]
fn default_fatal_fronts_share_exit_status_and_bypass_crt_exit_callbacks() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| matches!(s.name, "abort" | "_amsg_exit"))
        .collect();
    assert!(declarations.len() >= 20);
    for jit in [false, true] {
        for s in &declarations {
            let mut m = machine(jit);
            let module = s.module;
            let owner = if module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            let mut callback = Asm::new(CALLBACK);
            callback.bytes.extend_from_slice(&[0xff, 0x05]);
            callback
                .bytes
                .extend_from_slice(&(DATA + 0x800).to_le_bytes());
            callback.ret();
            m.cpu.write(CALLBACK, &callback.bytes).unwrap();
            let mut a = Asm::new(CODE);
            let onexit = if owner == "ucrtbase.dll" {
                "_crt_atexit"
            } else {
                "atexit"
            };
            // Process termination must leave every CRT callback untouched.
            let callback_owner = if signature::lookup(owner, onexit).is_some() {
                owner
            } else {
                "msvcr90.dll"
            };
            invoke(
                &mut m,
                &mut a,
                callback_owner,
                onexit,
                &[CALLBACK],
                DATA + 0x820,
            );
            let args = if s.name == "abort" { vec![] } else { vec![2] };
            invoke(&mut m, &mut a, module, s.name, &args, DATA + 0x824);
            a.bytes.extend_from_slice(&[0xc7, 0x05]);
            a.bytes.extend_from_slice(&(DATA + 0x804).to_le_bytes());
            a.bytes.extend_from_slice(&1u32.to_le_bytes());
            let result = finish(&mut m, a);
            let status = if s.name == "abort" { 3 } else { 255 };
            assert!(
                matches!(result, RunResult::Exited(value) if value == status),
                "{module}!{} {jit}: {result:?}",
                s.name
            );
            assert_eq!(
                m.cpu.read_u32(DATA + 0x800).unwrap(),
                0,
                "{module}!{} drained callbacks",
                s.name
            );
            assert_eq!(
                m.cpu.read_u32(DATA + 0x804).unwrap(),
                0,
                "{module}!{} returned",
                s.name
            );
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-15
#[test]
fn debugger_notifications_have_no_recipient_and_preserve_error_owners() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| s.name == "_crt_debugger_hook")
        .collect();
    assert!(declarations.len() >= 5);
    for jit in [false, true] {
        for s in &declarations {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            let owner = if s.module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                s.module
            };
            invoke(&mut m, &mut a, owner, "_errno", &[], DATA + 0x800);
            a.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
            a.push_imm(73)
                .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
            for (i, argument) in [0, 2, u32::MAX].into_iter().enumerate() {
                a.bytes.extend_from_slice(&[0xb8, 0x78, 0x56, 0x34, 0x12]);
                invoke(
                    &mut m,
                    &mut a,
                    s.module,
                    s.name,
                    &[argument],
                    DATA + 0x810 + i as u32 * 4,
                );
            }
            a.mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x830);
            a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                .mov_mem_eax(DATA + 0x834);
            assert!(
                matches!(finish(&mut m, a), RunResult::Exited(0)),
                "{} {jit}",
                s.module
            );
            for i in 0..3 {
                assert_eq!(m.cpu.read_u32(DATA + 0x810 + i * 4).unwrap(), 0x12345678);
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x830).unwrap(), 79);
            assert_eq!(m.cpu.read_u32(DATA + 0x834).unwrap(), 73);
            no_gaps(&m);
        }
    }
}
