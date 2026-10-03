//! Assembly of the Win32 font front and the raster/DirectDraw DC provider.
use adv_machine::{Env, Machine};
pub(crate) use adv_win32::gdi32::font::register;
use adv_win32::gdi32::font::{DrawingProvider, set_drawing_provider};

pub(crate) fn install<B: adv_cpu::backend::Backend>(machine: &mut Machine<B>) {
    fn contains_dc(env: &Env<'_>, dc: u32) -> bool {
        env.ext
            .get::<crate::Graphics<crate::Snapshot>>()
            .owns_dc(dc)
    }
    fn contains_bitmap(env: &Env<'_>, bitmap: u32) -> bool {
        env.ext
            .get::<crate::Graphics<crate::Snapshot>>()
            .owns_bitmap(bitmap)
    }
    set_drawing_provider(
        machine,
        DrawingProvider {
            contains_dc,
            contains_bitmap,
            call: crate::graphics::graphics_call,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_abi::Registry;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
    use adv_machine::{Clock, Outcome};
    use adv_win32::{Win32, Win32Config};

    #[test]
    fn font_decorators_preserve_the_raster_descriptor_and_trace() {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        crate::graphics::register_graphics(&mut registry);
        let names = ["SelectObject", "DeleteObject", "DeleteDC"];
        for name in names {
            registry.set_trace("gdi32.dll", name, true);
        }
        let before = names.map(|name| registry.get("gdi32.dll", name).unwrap());
        register(&mut registry);
        for (name, original) in names.into_iter().zip(before) {
            let decorated = registry.get("gdi32.dll", name).unwrap();
            assert_ne!(
                original.data, 0,
                "the raster front must have a bound operation"
            );
            assert_eq!(decorated.conv, original.conv);
            assert_eq!(decorated.data, original.data);
            assert_eq!(decorated.trace, original.trace);
        }
        assert!(registry.signature_conflicts().is_empty());
    }

    fn invoke(machine: &mut Machine<JitCpu>, data: u32, name: &str, args: &[u32]) -> u32 {
        let address = machine.resolve_import("gdi32.dll", name).unwrap();
        let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        machine.cpu.set_reg(Reg::Esp, data + 2048);
        for (i, &arg) in args.iter().enumerate() {
            machine
                .cpu
                .write_u32(data + 2052 + i as u32 * 4, arg)
                .unwrap();
        }
        let result = handler(&mut Env {
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: index,
        });
        let Outcome::Return(value) = result else {
            panic!("font/raster call must return")
        };
        value
    }

    #[test]
    fn exhausted_external_dc_ownership_rolls_back_the_raster_context() {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        crate::graphics::register_graphics(&mut registry);
        register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(".", "dc-limit.exe", "target/dc-limit-test"),
        )
        .unwrap();
        crate::graphics::install_graphics(&mut machine, 4, 2).unwrap();
        install(&mut machine);
        machine.sys.objects = adv_machine::Objects::with_limits(adv_machine::ObjectLimits {
            objects: 1,
            ..Default::default()
        });
        machine
            .spawn("dc limit", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let data = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        for _ in 0..8 {
            assert_eq!(invoke(&mut machine, data, "CreateCompatibleDC", &[0]), 0);
        }
        assert_eq!(machine.sys.objects.object_count(), 1);
        let graphics = machine
            .ext
            .get::<crate::graphics::Graphics<crate::graphics::Snapshot>>();
        for dc in 2..128 {
            assert!(!graphics.owns_dc(dc));
        }
    }

    #[test]
    fn font_and_bitmap_selection_share_the_raster_dc_lifetime() {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        crate::graphics::register_graphics(&mut registry);
        register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(".", "font-raster.exe", "target/font-raster-test"),
        )
        .unwrap();
        crate::graphics::install_graphics(&mut machine, 4, 2).unwrap();
        install(&mut machine);
        let data = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        machine
            .spawn(
                "font/raster test",
                adv_machine::layout::PARK_ADDR,
                &[],
                65536,
            )
            .unwrap();
        let dc = invoke(&mut machine, data, "CreateCompatibleDC", &[0]);
        assert_ne!(dc, 0);
        let mut header = [0; 40];
        for (at, value) in [(0, 40u32), (4, 4), (8, 2), (12, 0x200001)] {
            header[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        machine.cpu.write(data + 64, &header).unwrap();
        let bitmap = invoke(
            &mut machine,
            data,
            "CreateDIBSection",
            &[dc, data + 64, 0, data + 128, 0, 0],
        );
        assert_ne!(bitmap, 0);
        assert_eq!(
            invoke(&mut machine, data, "SelectObject", &[dc, bitmap]),
            adv_os::gdi::STOCK_BITMAP
        );
        let mut raw = [0; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[28..34].copy_from_slice(b"Arial\0");
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        assert_ne!(font, 0);
        assert_eq!(
            invoke(&mut machine, data, "SelectObject", &[dc, font]),
            0x0190_0010 + 13
        );
        assert_eq!(invoke(&mut machine, data, "SetPolyFillMode", &[dc, 2]), 1);
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 0);
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[bitmap]), 0);
        assert_eq!(
            invoke(&mut machine, data, "SelectObject", &[dc, 0x0190_0010 + 13]),
            font
        );
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 1);
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "SelectObject",
                &[dc, adv_os::gdi::STOCK_BITMAP]
            ),
            bitmap
        );
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[bitmap]), 1);
        assert_eq!(invoke(&mut machine, data, "DeleteDC", &[dc]), 1);
        assert!(
            machine
                .ext
                .get::<Win32>()
                .gdi
                .get(&machine.sys.objects, dc)
                .is_none()
        );
        assert_eq!(invoke(&mut machine, data, "GetPolyFillMode", &[dc]), 0);
    }
}
