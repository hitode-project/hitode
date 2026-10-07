//! Authored stream bytes and a real x86 ISequentialStream::Read vtable.
use super::*;
use iced_x86::code_asm::{CodeAssembler, dword_ptr, eax, ebx, ecx, edi, edx, esi, esp};

const PAYLOAD: u32 = 0x430000;
const CALLBACK: u32 = 0x450000;

fn put_word(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put_dword(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn bitmap(width: usize, height: i32, bpp: u16, pixel: impl Fn(usize, usize) -> u32) -> Vec<u8> {
    let colors = if bpp <= 8 { 1usize << bpp } else { 0 };
    let offset = 54 + colors * 4;
    let stride = (width * bpp as usize).div_ceil(32) * 4;
    let mut bytes = vec![0; offset + stride * height.unsigned_abs() as usize];
    put_word(&mut bytes, 0, 0x4d42);
    put_dword(&mut bytes, 2, offset as u32); // Native bfSize is the header size.
    put_dword(&mut bytes, 10, offset as u32);
    put_dword(&mut bytes, 14, 40);
    put_dword(&mut bytes, 18, width as u32);
    put_dword(&mut bytes, 22, height as u32);
    put_word(&mut bytes, 26, 1);
    put_word(&mut bytes, 28, bpp);
    // Deliberately leave biSizeImage zero: readers derive the padded size.
    for index in 0..colors {
        bytes[54 + index * 4..58 + index * 4].copy_from_slice(&[index as u8, 40, 80, 201]);
    }
    for y in 0..height.unsigned_abs() as usize {
        let row = if height > 0 {
            height as usize - 1 - y
        } else {
            y
        };
        for x in 0..width {
            let value = pixel(x, y);
            let at = offset + row * stride;
            match bpp {
                1 => bytes[at + x / 8] |= ((value & 1) as u8) << (7 - x % 8),
                4 => bytes[at + x / 2] |= ((value & 15) as u8) << if x % 2 == 0 { 4 } else { 0 },
                8 => bytes[at + x] = value as u8,
                16 => {
                    bytes[at + x * 2..at + x * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes())
                }
                24 => bytes[at + x * 3..at + x * 3 + 3].copy_from_slice(&value.to_le_bytes()[..3]),
                32 => bytes[at + x * 4..at + x * 4 + 4].copy_from_slice(&value.to_le_bytes()),
                _ => unreachable!(),
            }
        }
    }
    bytes
}

fn list(width: u16, height: u16, count: u16, flags: u16, color: Vec<u8>, mask: Vec<u8>) -> Vec<u8> {
    let mut bytes = vec![0; 28];
    for (at, value) in [
        (0, 0x4c49),
        (2, 0x101),
        (4, count),
        (6, count + 1),
        (8, 4),
        (10, width),
        (12, height),
        (18, flags),
    ] {
        put_word(&mut bytes, at, value);
    }
    put_dword(&mut bytes, 14, u32::MAX);
    bytes[20..28].fill(255);
    bytes.extend(color);
    if flags & 1 != 0 {
        bytes.extend(mask);
    }
    if flags & 0x1000 != 0 {
        bytes.extend([0; 22]);
    }
    bytes
}

fn stream(test: &mut Fixture, bytes: &[u8], hresult: u32) -> u32 {
    test.machine.cpu.map(PAYLOAD, 131072, Prot::READ).unwrap();
    test.machine.cpu.write(PAYLOAD, bytes).unwrap();
    test.machine.cpu.map(CALLBACK, 4096, Prot::RX).unwrap();
    let table = test.word(0);
    test.word(0);
    test.word(0);
    test.word(CALLBACK);
    let state = test.word(table);
    for value in [PAYLOAD, bytes.len() as u32, 0, hresult, 0, 0, 0] {
        test.word(value);
    }
    let sleep = test
        .machine
        .resolve_import("kernel32.dll", "Sleep")
        .unwrap();
    let mut code = CodeAssembler::new(32).unwrap();
    if hresult == u32::MAX {
        let exit = test
            .machine
            .resolve_import("kernel32.dll", "ExitThread")
            .unwrap();
        code.push(0).unwrap();
        code.call(u64::from(exit)).unwrap();
    }
    code.push(1).unwrap();
    code.call(u64::from(sleep)).unwrap();
    code.push(esi).unwrap();
    code.push(edi).unwrap();
    code.push(ebx).unwrap();
    code.mov(ebx, dword_ptr(esp + 16)).unwrap(); // this
    code.inc(dword_ptr(ebx + 20)).unwrap();
    code.mov(ecx, dword_ptr(esp + 24)).unwrap(); // cb
    code.mov(edx, dword_ptr(ebx + 8)).unwrap();
    code.sub(edx, dword_ptr(ebx + 12)).unwrap();
    code.cmp(ecx, edx).unwrap();
    let mut enough = code.create_label();
    code.jbe(enough).unwrap();
    code.mov(ecx, edx).unwrap();
    code.set_label(&mut enough).unwrap();
    code.mov(eax, dword_ptr(esp + 28)).unwrap(); // pcbRead
    code.test(eax, eax).unwrap();
    let mut no_count = code.create_label();
    code.jz(no_count).unwrap();
    code.inc(dword_ptr(ebx + 24)).unwrap();
    code.mov(dword_ptr(eax), ecx).unwrap();
    code.set_label(&mut no_count).unwrap();
    code.mov(esi, dword_ptr(ebx + 4)).unwrap();
    code.add(esi, dword_ptr(ebx + 12)).unwrap();
    code.add(dword_ptr(ebx + 12), ecx).unwrap();
    code.mov(edi, dword_ptr(esp + 20)).unwrap();
    code.cld().unwrap();
    code.rep().movsb().unwrap();
    code.mov(eax, dword_ptr(ebx + 16)).unwrap();
    code.mov(edx, dword_ptr(ebx + 20)).unwrap();
    code.cmp(edx, dword_ptr(ebx + 28)).unwrap();
    let mut not_failed = code.create_label();
    code.jne(not_failed).unwrap();
    code.mov(eax, 0x80004005u32).unwrap();
    code.set_label(&mut not_failed).unwrap();
    code.pop(ebx).unwrap();
    code.pop(edi).unwrap();
    code.pop(esi).unwrap();
    code.ret_1(16).unwrap();
    let bytes = code.assemble(u64::from(CALLBACK)).unwrap();
    test.machine.cpu.write(CALLBACK, &bytes).unwrap();
    test.machine
        .cpu
        .executor_mut()
        .add_region(Region::new(CALLBACK, bytes.len()))
        .unwrap();
    state
}

#[test]
fn legacy_stream_calls_guest_read_with_null_count_and_keeps_four_column_order() {
    for jit in [false, true] {
        for bottom_up in [false, true] {
            let mut test = Fixture::new(jit);
            let color = bitmap(12, if bottom_up { 4 } else { -4 }, 32, |x, y| {
                0x80000000 | ((y / 2 * 4 + x / 3 + 1) as u32 * 0x010203)
            });
            let mask = bitmap(12, if bottom_up { 4 } else { -4 }, 1, |x, y| {
                ((x + y) % 2) as u32
            });
            let bytes = list(3, 2, 5, 0x10ff, color, mask);
            let state = stream(&mut test, &bytes, 1); // S_FALSE is successful.
            test.call("kernel32.dll!SetLastError", &[I(1234)]);
            let result = test.call("ImageList_Read", &[I(state)]);
            let error = test.error();
            let cx = test.word(0);
            let cy = test.word(0);
            let size = test.call("ImageList_GetIconSize", &[M(result), I(cx), I(cy)]);
            test.run();
            assert_eq!(
                (test.value(size), test.value(cx), test.value(cy)),
                (1, 3, 2)
            );
            assert_eq!(test.value(error), 1234);
            assert_eq!(test.value(state + 12), bytes.len() as u32);
            assert_eq!(test.value(state + 20), 9);
            assert_eq!(test.value(state + 24), 0);
            let images = &test.list(result).images;
            assert_eq!(images.len(), 5);
            for (index, image) in images.iter().enumerate() {
                assert_eq!(
                    image.rgba,
                    [
                        index as u8 + 1,
                        (index as u8 + 1) * 2,
                        (index as u8 + 1) * 3,
                        128
                    ]
                    .repeat(6)
                );
                let left = index % 4 * 3;
                assert_eq!(
                    image.mask.as_ref().unwrap(),
                    &(0..2)
                        .flat_map(|y| (0..3).map(move |x| ((left + x + y) % 2) as u8))
                        .collect::<Vec<_>>()
                );
            }
            assert!(
                test.machine
                    .ext
                    .get::<adv_abi::ApiDiagnostics>()
                    .warnings
                    .is_empty()
            );
        }
    }
}

#[test]
fn legacy_stream_decodes_palettes_rgb555_rgb24_and_zero_alpha_with_mask() {
    for jit in [false, true] {
        for (bpp, value, expected) in [
            (1, 1, [80, 40, 1, 255]),
            (4, 13, [80, 40, 13, 255]),
            (8, 200, [80, 40, 200, 255]),
            (16, 0x7c1f, [255, 0, 255, 255]),
            (24, 0x14283c, [20, 40, 60, 255]),
            (32, 0x14283c, [20, 40, 60, 255]),
        ] {
            let mut test = Fixture::new(jit);
            let color = bitmap(12, 2, bpp, |_, _| value);
            let mask = bitmap(12, 2, 1, |x, _| u32::from(x % 3 == 1));
            let bytes = list(3, 2, 1, bpp | 1, color, mask);
            let state = stream(&mut test, &bytes, 0);
            let result = test.call("ImageList_Read", &[I(state)]);
            test.run();
            let image = &test.list(result).images[0];
            let mut transparent = expected;
            transparent[3] = 0;
            assert_eq!(
                image.rgba,
                [
                    expected,
                    transparent,
                    expected,
                    expected,
                    transparent,
                    expected
                ]
                .concat()
            );
            assert_eq!(image.mask.as_ref().unwrap(), &[0, 1, 0, 0, 1, 0]);
        }
    }
}

#[test]
fn failed_malformed_and_oversized_streams_publish_no_list() {
    for jit in [false, true] {
        for case in 0..6 {
            let mut test = Fixture::new(jit);
            let mut bytes = list(3, 2, 1, 0x20, bitmap(12, 2, 32, |_, _| 0xffffffff), vec![]);
            let hresult = if case == 0 { 0x80004005 } else { 0 };
            match case {
                1 => bytes.truncate(12), // A successful short header gets a zeroed tail.
                2 => put_word(&mut bytes, 2, 0x600),
                3 => put_dword(&mut bytes, 28 + 18, 0x7fffffff), // bounded before pixel allocation
                4 => put_dword(&mut bytes, 28 + 30, 1),          // BI_RLE8 is unavailable
                _ => (),
            }
            let state = stream(&mut test, &bytes, hresult);
            if case == 5 {
                test.machine.cpu.write_u32(state + 28, 4).unwrap();
            }
            let sibling = test.call("ImageList_Create", &[I(3), I(2), I(0x20), I(0), I(1)]);
            let result = test.call("ImageList_Read", &[I(state)]);
            test.run();
            assert_eq!(test.value(result), 0);
            assert!(test.list(sibling).images.is_empty());
            let lists = test.machine.ext.get::<ImageLists>();
            assert_eq!(
                test.machine
                    .sys
                    .objects
                    .alias_count(lists.namespace.unwrap()),
                1
            );
        }
    }
}

#[test]
fn large_stream_reads_in_bounded_chunks_and_consumes_the_extended_trailer() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let color = bitmap(48, 36, 32, |x, y| 0xff000000 | (x + y * 48) as u32);
        let mask = bitmap(48, 36, 1, |_, _| 0);
        let bytes = list(12, 12, 8, 0x10ff, color, mask);
        let state = stream(&mut test, &bytes, 1);
        let result = test.call("ImageList_Read", &[I(state)]);
        test.run();
        assert_eq!(test.value(state + 12), bytes.len() as u32);
        assert_eq!(test.value(state + 20), 10);
        let images = &test.list(result).images;
        assert_eq!(images.len(), 8);
        for (index, image) in images.iter().enumerate() {
            for y in 0..12 {
                for x in 0..12 {
                    let value = (index % 4 * 12 + x + (index / 4 * 12 + y) * 48) as u32;
                    let [blue, green, red, _] = value.to_le_bytes();
                    assert_eq!(
                        &image.rgba[(y * 12 + x) * 4..(y * 12 + x + 1) * 4],
                        &[red, green, blue, 255]
                    );
                }
            }
        }
    }
}

#[test]
fn exiting_inside_guest_read_drops_scratch_without_process_heap_allocations() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let state = stream(&mut test, &[0; 28], u32::MAX);
        let sibling = test.call("ImageList_Create", &[I(3), I(2), I(0x20), I(0), I(1)]);
        let result = test.call("ImageList_Read", &[I(state)]);
        let heaps = test.machine.ext.get_mut::<adv_os::Heaps>();
        let heap = heaps.process_heap;
        let first_free = heaps
            .alloc(
                &mut test.machine.cpu,
                &mut test.machine.sys.vm,
                heap,
                4096,
                false,
            )
            .unwrap();
        assert!(heaps.free(
            &mut test.machine.cpu,
            &mut test.machine.sys.vm,
            heap,
            first_free
        ));
        test.run();
        assert_eq!(test.value(result), 0);
        assert!(test.list(sibling).images.is_empty());
        let heaps = test.machine.ext.get_mut::<adv_os::Heaps>();
        let after_exit = heaps
            .alloc(
                &mut test.machine.cpu,
                &mut test.machine.sys.vm,
                heap,
                4096,
                false,
            )
            .unwrap();
        assert_eq!(after_exit, first_free);
        assert!(heaps.free(
            &mut test.machine.cpu,
            &mut test.machine.sys.vm,
            heap,
            after_exit
        ));
    }
}
