use super::*;

fn snapshot(a: &mut Asm, slot: u32, output: u32) {
    a.mov_eax_mem(slot).mov_eax_ptr_eax().mov_mem_eax(output);
}
fn backing(m: &Machine<JitCpu>, pointer: u32) -> u32 {
    m.cpu.read_u32((pointer & !3) - 4).unwrap()
}

// spec: CRT-LIB-9, CRT-LIB-10
#[test]
fn aligned_views_resize_zero_and_release_real_backings_across_all_versions() {
    for jit in [false, true] {
        for module in [
            "msvcrt.dll",
            "msvcr70.dll",
            "msvcr71.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
        ] {
            let extended = !matches!(module, "msvcrt.dll" | "msvcr70.dll" | "msvcr71.dll");
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_malloc",
                &[V(37), V(16)],
                DATA,
            );
            store_word(&mut a, DATA, 0x12345678);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_realloc",
                &[S(DATA), V(73), V(64)],
                DATA + 4,
            );
            snapshot(&mut a, DATA + 4, DATA + 8);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_malloc",
                &[V(37), V(64), V(5)],
                DATA + 12,
            );
            store_word(&mut a, DATA + 12, 0xfedcba98);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_realloc",
                &[S(DATA + 12), V(79), V(128), V(5)],
                DATA + 16,
            );
            snapshot(&mut a, DATA + 16, DATA + 20);
            if extended {
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_recalloc",
                    &[S(DATA + 4), V(12), V(10), V(64)],
                    DATA + 24,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_msize",
                    &[S(DATA + 24), V(64), V(0)],
                    DATA + 28,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_offset_recalloc",
                    &[S(DATA + 16), V(11), V(10), V(128), V(5)],
                    DATA + 32,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_msize",
                    &[S(DATA + 32), V(128), V(5)],
                    DATA + 36,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_recalloc",
                    &[V(0), V(8), V(4), V(32)],
                    DATA + 40,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_offset_recalloc",
                    &[V(0), V(8), V(4), V(16), V(3)],
                    DATA + 44,
                );
            }
            // Native aligned allocation accepts an empty view with offset zero.
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_malloc",
                &[V(0), V(1)],
                DATA + 48,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_malloc",
                &[V(0), V(2), V(0)],
                DATA + 52,
            );
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            let word = |n| m.cpu.read_u32(DATA + n).unwrap();
            assert_eq!(word(8), 0x12345678);
            assert_eq!(word(20), 0xfedcba98);
            let ordinary = word(if extended { 24 } else { 4 });
            let offset = word(if extended { 32 } else { 16 });
            assert_ne!(ordinary, 0);
            assert_ne!(offset, 0);
            assert_eq!(ordinary % 64, 0);
            assert_eq!((offset + 5) % 128, 0);
            let heap = m.ext.get::<Heaps>().process_heap;
            let mut live = vec![ordinary, offset, word(48), word(52)];
            if extended {
                assert_eq!(word(28), 120);
                assert_eq!(word(36), 110);
                for (pointer, start, length) in [
                    (ordinary, 73, 47),
                    (offset, 79, 31),
                    (word(40), 0, 32),
                    (word(44), 0, 32),
                ] {
                    let mut bytes = vec![0x77; length];
                    m.cpu.read(pointer + start, &mut bytes).unwrap();
                    assert!(bytes.iter().all(|&b| b == 0), "{module}: {bytes:?}");
                }
                assert_eq!(m.cpu.read_u32(ordinary).unwrap(), 0x12345678);
                assert_eq!(m.cpu.read_u32(offset).unwrap(), 0xfedcba98);
                assert_eq!(word(40) % 32, 0);
                assert_eq!((word(44) + 3) % 16, 0);
                live.extend([word(40), word(44)]);
            }
            let bases: Vec<_> = live
                .iter()
                .map(|&p| {
                    assert_ne!(p, 0);
                    let base = backing(&m, p);
                    assert_eq!(m.ext.get::<Heaps>().owner(&m.cpu, base), Some(heap));
                    assert!(base < p);
                    base
                })
                .collect();
            // Releasing through aligned fronts must free the recovered base.
            let mut a = Asm::new(CODE);
            for &pointer in &live {
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_free",
                    &[V(pointer)],
                    DATA + 64,
                );
            }
            invoke(&mut m, &mut a, module, "_aligned_free", &[V(0)], DATA + 68);
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            assert!(matches!(run(&mut m, a), RunResult::Exited(0)));
            for base in bases {
                assert_eq!(m.ext.get::<Heaps>().owner(&m.cpu, base), None);
            }
            clean(&m);
        }
    }
}

// spec: CRT-LIB-9, CRT-LIB-10, CRT-LIB-8
#[test]
fn aligned_failures_preserve_contents_and_apply_version_validation_before_effects() {
    for jit in [false, true] {
        for (module, modern, zero_alignment) in [
            ("msvcr70.dll", false, false),
            ("msvcr80.dll", true, true),
            ("msvcr90.dll", true, true),
            ("ucrtbase.dll", true, false),
            ("api-ms-win-crt-heap-l1-1-0.dll", true, false),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            if modern {
                let mut handler = Asm::new(CALLBACK);
                handler.bytes.extend_from_slice(&[0xff, 0x05]);
                handler.bytes.extend_from_slice(&(DATA + 128).to_le_bytes());
                handler.ret();
                m.cpu.write(CALLBACK, &handler.bytes).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    owner(module),
                    "_set_invalid_parameter_handler",
                    &[V(CALLBACK)],
                    DATA + 120,
                );
            }
            invoke(&mut m, &mut a, owner(module), "_errno", &[], ERRNO);
            store_word(&mut a, ERRNO, 79);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_malloc",
                &[V(32), V(16)],
                DATA,
            );
            store_word(&mut a, DATA, 0xdead1234);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_realloc",
                &[S(DATA), V(u32::MAX), V(16)],
                DATA + 4,
            );
            snapshot(&mut a, ERRNO, DATA + 8);
            snapshot(&mut a, DATA, DATA + 12);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_malloc",
                &[V(32), V(3)],
                DATA + 16,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_malloc",
                &[V(32), V(16), V(32)],
                DATA + 20,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_malloc",
                &[V(0), V(16), V(1)],
                DATA + 24,
            );
            snapshot(&mut a, ERRNO, DATA + 28);
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_malloc",
                &[V(32), V(0)],
                DATA + 32,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_malloc",
                &[V(32), V(0), V(1)],
                DATA + 36,
            );
            a.mov_eax_mem(DATA);
            a.bytes
                .extend_from_slice(&[0x83, 0xe0, 0xfc, 0x8b, 0x40, 0xfc]);
            a.mov_mem_eax(DATA + 56);
            // Free-on-zero comes before the deliberately invalid geometry.
            invoke(
                &mut m,
                &mut a,
                module,
                "_aligned_offset_realloc",
                &[S(DATA), V(0), V(3), V(999)],
                DATA + 40,
            );
            if modern {
                // Multiplication overflow precedes all pointer/geometry access.
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_offset_recalloc",
                    &[V(0x1234), V(u32::MAX), V(2), V(3), V(999)],
                    DATA + 44,
                );
                snapshot(&mut a, ERRNO, DATA + 48);
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_aligned_msize",
                    &[V(0), V(16), V(0)],
                    DATA + 52,
                );
            }
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            let word = |n| m.cpu.read_u32(DATA + n).unwrap();
            assert_eq!(word(4), 0);
            assert_eq!(word(8), 12);
            assert_eq!(word(12), 0xdead1234);
            for n in [16, 20, 24, 40] {
                assert_eq!(word(n), 0);
            }
            assert_eq!(word(28), 22);
            assert_eq!(m.ext.get::<Heaps>().owner(&m.cpu, word(56)), None);
            if zero_alignment {
                assert_ne!(word(32), 0);
                assert_ne!(word(36), 0);
                assert_eq!(word(32) % 4, 0);
                assert_eq!((word(36) + 1) % 4, 0);
            } else {
                assert_eq!((word(32), word(36)), (0, 0));
            }
            if modern {
                assert_eq!(word(44), 0);
                assert_eq!(word(48), 12);
                assert_eq!(word(52), u32::MAX);
                assert_eq!(word(128), if zero_alignment { 4 } else { 6 });
            } else {
                assert_eq!(word(128), 0);
            }
            clean(&m);
        }
    }
}
