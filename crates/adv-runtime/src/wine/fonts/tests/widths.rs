use super::*;

// spec: WIN32-GDI-12
#[test]
fn unicode_width_output_is_atomic_for_bad_ranges_dc_and_page_boundaries() {
    let (mut machine, data, dc) = fixture();
    let output = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(output, &[0xcd; 4096]).unwrap();
    machine
        .cpu
        .protect(output + 4096, 4096, Prot::READ)
        .unwrap();
    for args in [
        [dc, 2, 1, output],
        [dc, 0, 65536, output],
        [0, 0, 0, output],
        [dc, 0, 0, 0],
        [dc, 0, 1, output + 4092],
    ] {
        assert_eq!(invoke(&mut machine, data, "GetCharWidth32W", &args), 0);
        let mut actual = [0; 4096];
        machine.cpu.read(output, &mut actual).unwrap();
        assert_eq!(actual, [0xcd; 4096]);
    }
    machine.cpu.protect(output, 4096, Prot::READ).unwrap();
    assert_eq!(
        invoke(&mut machine, data, "GetCharWidth32W", &[dc, 0, 1, output]),
        0
    );
    let mut actual = [0; 4096];
    machine.cpu.read(output, &mut actual).unwrap();
    assert_eq!(actual, [0xcd; 4096]);
}
