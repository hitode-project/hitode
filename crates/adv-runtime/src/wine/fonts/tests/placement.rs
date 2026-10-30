use super::*;

// spec: WIN32-GDI-11
#[test]
fn placement_rejects_bad_optional_arrays_before_any_output_changes() {
    let (mut machine, data, dc) = fixture();
    machine.cpu.write(data + 128, &65u16.to_le_bytes()).unwrap();
    let buffer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write_u32(buffer + 4092, 0xcdcdcdcd).unwrap();
    machine
        .cpu
        .protect(buffer + 4096, 4096, Prot::READ)
        .unwrap();
    let fields = [36, 0, data + 640, buffer + 4092, 0, 0, 0, 2, 0x12345678];
    machine.cpu.write(data + 640, &[0xcd; 8]).unwrap();
    for (index, word) in fields.into_iter().enumerate() {
        machine
            .cpu
            .write_u32(data + 512 + index as u32 * 4, word)
            .unwrap();
    }
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharacterPlacementW",
            &[dc, data + 128, 1, 100, data + 512, 0]
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 640).unwrap(), 0xcdcdcdcd);
    assert_eq!(machine.cpu.read_u32(data + 540).unwrap(), 2);
    assert_eq!(machine.cpu.read_u32(buffer + 4092).unwrap(), 0xcdcdcdcd);
}
