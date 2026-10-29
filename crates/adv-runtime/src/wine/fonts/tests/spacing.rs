use super::*;

// spec: WIN32-GDI-10
#[test]
fn spacing_checks_entire_outputs_and_reads_overlapping_index_arrays_first() {
    let (mut machine, data, dc) = fixture();
    let buffer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(buffer, &[0xcd; 4096]).unwrap();
    machine
        .cpu
        .protect(buffer + 4096, 4096, Prot::READ)
        .unwrap();
    for args in [
        [dc, 0, 2, 0, buffer + 4092],
        [dc, 0, 65537, 0, buffer],
        [dc, 0, 1, u32::MAX, buffer],
        [0, 0, 1, 0, buffer],
        [dc, 0, 1, 0, 0],
    ] {
        assert_eq!(
            invoke(&mut machine, data, "GetCharABCWidthsI", &args),
            0,
            "{args:?}"
        );
        assert_eq!(machine.cpu.read_u32(buffer).unwrap(), 0xcdcdcdcd);
        assert_eq!(machine.cpu.read_u32(buffer + 4092).unwrap(), 0xcdcdcdcd);
    }
    machine.cpu.write(buffer, &[0, 0, 1, 0, 2, 0]).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharABCWidthsI",
            &[dc, 0, 3, buffer, data + 512]
        ),
        1
    );
    let mut expected = [0; 36];
    machine.cpu.read(data + 512, &mut expected).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharABCWidthsI",
            &[dc, 0, 3, buffer, buffer]
        ),
        1
    );
    let mut actual = [0; 36];
    machine.cpu.read(buffer, &mut actual).unwrap();
    assert_eq!(actual, expected);
}
