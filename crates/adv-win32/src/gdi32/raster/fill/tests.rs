use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_device::surface::{MemoryOperations, MemoryView, PixelFormat, Snapshot};
use adv_machine::{Clock, Machine};
use std::borrow::Cow;

fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut bytes = vec![0; length];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, source, dest, length| {
                let mut bytes = vec![0; length];
                cpu.read(source, &mut bytes).map_err(|e| e.to_string())?;
                cpu.write(dest, &bytes).map_err(|e| e.to_string())
            },
        },
    )
}
fn fixture(
    width: u32,
    height: u32,
    format: PixelFormat,
    bottom_up: bool,
) -> (Machine<JitCpu>, u32, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "fill-rect.exe", "target/fill-rect-tests"),
    )
    .unwrap();
    machine
        .spawn("fill", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 16384, Prot::RW)
        .unwrap();
    let mut store = SurfaceStore::new(width, height, arena, 16384, Snapshot::default()).unwrap();
    let target = store
        .create(
            width,
            height,
            bottom_up,
            format,
            &mut memory(&mut machine.cpu),
        )
        .unwrap();
    let pixels = store.get(target).unwrap().pixels;
    let dc = store.new_id();
    let gdi = machine.ext.get::<Win32>().gdi.clone();
    let mut contexts = adv_os::gdi::DeviceContexts::new(gdi);
    assert!(contexts.create(&mut machine.sys.objects, dc, target));
    machine.ext.insert(store);
    super::super::front::set_provider(
        &mut machine.ext,
        super::super::front::DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    (machine, data, dc, pixels)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn invoke(
    machine: &mut Machine<JitCpu>,
    data: u32,
    dc: u32,
    rectangle: [i32; 4],
    brush: u32,
) -> u32 {
    machine
        .cpu
        .write(
            data,
            &rectangle
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    match fill(
        &mut env(machine),
        Handle(dc),
        GuestPtr::new(data),
        Handle(brush),
    ) {
        Outcome::Return(value) => value,
        Outcome::Fail(error) => panic!("{error}"),
        _ => panic!("memory DC must return immediately"),
    }
}

#[test]
fn native_white_custom_null_empty_and_reversed_rectangles_match_dib_pixels() {
    let (mut machine, data, dc, pixels) = fixture(4, 4, PixelFormat::Bgra32, false);
    machine.cpu.write(pixels, &[0xa5; 64]).unwrap();
    let mut gdi = machine.ext.get::<Win32>().gdi.clone();
    let brush = gdi.add(
        &mut machine.sys.objects,
        GdiObj::Brush { color: 0x0056_3412 },
    );
    assert_ne!(brush, 0);
    assert_eq!(
        invoke(&mut machine, data, dc, [0, 0, 9999, 9999], stock_object(0)),
        1
    );
    assert_eq!(invoke(&mut machine, data, dc, [1, 1, 3, 3], brush), 1);
    assert_eq!(
        invoke(&mut machine, data, dc, [1, 1, 3, 3], stock_object(5)),
        1
    );
    assert_eq!(invoke(&mut machine, data, dc, [0, 0, 0, 3], brush), 1);
    assert_eq!(invoke(&mut machine, data, dc, [3, 3, 1, 1], brush), 1);
    let mut packed = [0; 64];
    machine.cpu.read(pixels, &mut packed).unwrap();
    for (index, pixel) in packed.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            u32::from_le_bytes(*pixel),
            if [5, 6, 9, 10].contains(&index) {
                0x0012_3456
            } else {
                0x00ff_ffff
            }
        );
    }
    let Some(GdiObj::Dc { selected, .. }) = gdi.get(&machine.sys.objects, dc) else {
        panic!("DC")
    };
    assert_eq!(
        selected[0],
        stock_object(0),
        "FillRect does not select its brush"
    );
    assert_eq!(machine.ext.get::<SurfaceStore>().stats.presentations, 0);
}

#[test]
fn clipped_fill_respects_16_24_32_bit_pitch_bottom_up_and_unrelated_bytes() {
    for (format, encoded) in [
        (PixelFormat::Rgb555, vec![0xca, 0x08]),
        (PixelFormat::Rgb565, vec![0xaa, 0x11]),
        (PixelFormat::Bgr24, vec![0x56, 0x34, 0x12]),
        (PixelFormat::Bgra32, vec![0x56, 0x34, 0x12, 0]),
    ] {
        for bottom_up in [false, true] {
            let (mut machine, data, dc, pixels) = fixture(3, 3, format, bottom_up);
            let surface = machine.ext.get::<SurfaceStore>().get(16).unwrap();
            let pitch = surface.pitch;
            let length = pitch * 3;
            machine
                .cpu
                .write(pixels, &vec![0xa5; length as usize])
                .unwrap();
            let mut gdi = machine.ext.get::<Win32>().gdi.clone();
            let brush = gdi.add(
                &mut machine.sys.objects,
                GdiObj::Brush { color: 0x0056_3412 },
            );
            assert_ne!(brush, 0);
            assert_eq!(invoke(&mut machine, data, dc, [-9, 0, 2, 1], brush), 1);
            let mut actual = vec![0; length as usize];
            machine.cpu.read(pixels, &mut actual).unwrap();
            let mut expected = vec![0xa5; length as usize];
            let row = if bottom_up { 2 * pitch } else { 0 } as usize;
            expected[row..row + encoded.len()].copy_from_slice(&encoded);
            expected[row + encoded.len()..row + 2 * encoded.len()].copy_from_slice(&encoded);
            assert_eq!(actual, expected, "{format:?}, bottom_up={bottom_up}");
        }
    }
}

#[test]
fn a_later_readonly_row_rejects_before_mutating_writable_rows() {
    let (mut machine, data, dc, pixels) = fixture(256, 5, PixelFormat::Bgra32, false);
    assert_eq!(pixels & 4095, 0);
    machine.cpu.write(pixels, &[0xa5; 5120]).unwrap();
    machine
        .cpu
        .protect(pixels + 4096, 4096, Prot::READ)
        .unwrap();
    assert_eq!(
        invoke(&mut machine, data, dc, [0, 0, 256, 5], stock_object(0)),
        0
    );
    let mut actual = [0; 5120];
    machine.cpu.read(pixels, &mut actual).unwrap();
    assert_eq!(actual, [0xa5; 5120]);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
    assert_eq!(
        invoke(&mut machine, data, dc, [0, 0, 1, 1], stock_object(0)),
        1
    );
    assert_eq!(machine.cpu.read_u32(pixels).unwrap(), 0xffffff);
}
