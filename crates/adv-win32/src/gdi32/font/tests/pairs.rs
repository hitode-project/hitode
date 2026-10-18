#[cfg(windows)]
use super::*;

#[test]
#[cfg(windows)]
// spec: WIN32-GDI-12
fn ansi_kerning_filters_and_packs_the_same_pairs_as_the_native_selected_font() {
    let (mut machine, data, dc) = fixture();
    machine.ext.get_mut::<Win32>().codec =
        adv_os::Codec::new(unsafe { windows_sys::Win32::Globalization::GetACP() });
    let native = native_font(&machine, dc);
    let count = unsafe { gdi::GetKerningPairsA(native.dc, 0, std::ptr::null_mut()) };
    assert!(count > 0);
    let mut expected: Vec<gdi::KERNINGPAIR> =
        (0..count).map(|_| unsafe { std::mem::zeroed() }).collect();
    assert_eq!(
        unsafe { gdi::GetKerningPairsA(native.dc, count, expected.as_mut_ptr()) },
        count
    );
    assert_eq!(
        invoke(&mut machine, data, "GetKerningPairsA", &[dc, 0, 0]),
        count
    );
    let output = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, count * 8 + 16, Prot::RW)
        .unwrap();
    for capacity in [1, 7, count, count + 1] {
        machine
            .cpu
            .write(output, &vec![0xcc; count as usize * 8 + 16])
            .unwrap();
        let copied = capacity.min(count);
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetKerningPairsA",
                &[dc, capacity, output]
            ),
            copied
        );
        for (index, pair) in expected.iter().take(copied as usize).enumerate() {
            let pointer = output + index as u32 * 8;
            assert_eq!(machine.cpu.read_u16(pointer).unwrap(), pair.wFirst);
            assert_eq!(machine.cpu.read_u16(pointer + 2).unwrap(), pair.wSecond);
            assert_eq!(
                machine.cpu.read_u32(pointer + 4).unwrap() as i32,
                pair.iKernAmount
            );
        }
        assert_eq!(
            machine.cpu.read_u32(output + copied * 8).unwrap(),
            0xcccccccc
        );
    }
}
