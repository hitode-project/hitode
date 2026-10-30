use super::*;

// spec: WIN32-GDI-12
#[test]
fn outline_metrics_preflight_prevents_partial_guest_writes() {
    let (mut machine, data, dc) = fixture();
    let pages = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    let readonly = pages + 4096;
    machine.cpu.protect(readonly, 4096, Prot::READ).unwrap();
    let output = readonly - 96;
    for pointer in [readonly, u32::MAX - 3, output] {
        machine.cpu.write(output, &[0xa5; 96]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetOutlineTextMetricsA",
                &[dc, 212, pointer]
            ),
            0
        );
        let mut unchanged = [0; 96];
        machine.cpu.read(output, &mut unchanged).unwrap();
        assert_eq!(unchanged, [0xa5; 96]);
    }
}
