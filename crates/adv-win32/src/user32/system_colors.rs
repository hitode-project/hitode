//! A fixed virtual classic theme, independent of the host's theme and settings.
//! System brushes use the existing GDI namespace and process object ownership.
use adv_abi::{Env, Handle, Registry, win32};

use crate::{Win32, gdi32::GdiObj};

const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
}

// COLORREF uses 0x00BBGGRR. Slot 25 is the SDK's unused color index.
const COLORS: [u32; 31] = [
    rgb(212, 208, 200), // COLOR_SCROLLBAR
    rgb(58, 110, 165),  // COLOR_BACKGROUND / DESKTOP
    rgb(10, 36, 106),   // COLOR_ACTIVECAPTION
    rgb(128, 128, 128), // COLOR_INACTIVECAPTION
    rgb(212, 208, 200), // COLOR_MENU
    rgb(255, 255, 255), // COLOR_WINDOW
    rgb(0, 0, 0),       // COLOR_WINDOWFRAME
    rgb(0, 0, 0),       // COLOR_MENUTEXT
    rgb(0, 0, 0),       // COLOR_WINDOWTEXT
    rgb(255, 255, 255), // COLOR_CAPTIONTEXT
    rgb(212, 208, 200), // COLOR_ACTIVEBORDER
    rgb(212, 208, 200), // COLOR_INACTIVEBORDER
    rgb(128, 128, 128), // COLOR_APPWORKSPACE
    rgb(10, 36, 106),   // COLOR_HIGHLIGHT
    rgb(255, 255, 255), // COLOR_HIGHLIGHTTEXT
    rgb(212, 208, 200), // COLOR_BTNFACE / 3DFACE
    rgb(128, 128, 128), // COLOR_BTNSHADOW / 3DSHADOW
    rgb(128, 128, 128), // COLOR_GRAYTEXT
    rgb(0, 0, 0),       // COLOR_BTNTEXT
    rgb(212, 208, 200), // COLOR_INACTIVECAPTIONTEXT
    rgb(255, 255, 255), // COLOR_BTNHIGHLIGHT / 3DHIGHLIGHT
    rgb(64, 64, 64),    // COLOR_3DDKSHADOW
    rgb(212, 208, 200), // COLOR_3DLIGHT
    rgb(0, 0, 0),       // COLOR_INFOTEXT
    rgb(255, 255, 225), // COLOR_INFOBK
    0,                  // unused
    rgb(0, 0, 255),     // COLOR_HOTLIGHT
    rgb(166, 202, 240), // COLOR_GRADIENTACTIVECAPTION
    rgb(192, 192, 192), // COLOR_GRADIENTINACTIVECAPTION
    rgb(49, 106, 197),  // COLOR_MENUHILIGHT
    rgb(212, 208, 200), // COLOR_MENUBAR
];

// Fixed system aliases are distinct from GetStockObject aliases and ordinary
// allocated GDI handles. The common object table remains the only owner/cache.
const BRUSH_BASE: u32 = 0x0190_0100;

fn color(index: i32) -> Option<u32> {
    let index = usize::try_from(index).ok()?;
    if index == 25 {
        return None;
    }
    COLORS.get(index).copied()
}

pub(crate) fn is_system_brush(handle: u32) -> bool {
    handle
        .checked_sub(BRUSH_BASE)
        .and_then(|index| i32::try_from(index).ok())
        .and_then(color)
        .is_some()
}

#[win32(user32, name = "GetSysColor", level = state)]
fn get_sys_color(_env: &mut Env<'_>, index: i32) -> u32 {
    color(index).unwrap_or(0)
}

#[win32(user32, name = "GetSysColorBrush", level = state)]
fn get_sys_color_brush(env: &mut Env<'_>, index: i32) -> Handle {
    let Some(color) = color(index) else {
        return Handle(0);
    };
    let handle = BRUSH_BASE + u32::try_from(index).unwrap();
    let gdi = &mut env.ext.get_mut::<Win32>().gdi;
    if gdi.get(&env.sys.objects, handle).is_some()
        || gdi.add_at(&mut env.sys.objects, handle, GdiObj::Brush { color })
    {
        Handle(handle)
    } else {
        Handle(0)
    }
}

pub fn register(registry: &mut Registry) {
    registry.register(get_sys_color_ABI);
    registry.register(get_sys_color_brush_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[test]
    fn system_brush_is_cached_matches_color_and_survives_delete() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(
                "/nonexistent/system-colors",
                "query.exe",
                "/nonexistent/system-colors-data",
            ),
        )
        .unwrap();
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        for index in [0, 5, 8, 13, 15, 24, 26, 30] {
            let brush = get_sys_color_brush(&mut env, index).0;
            assert_ne!(brush, 0);
            let count = env.sys.objects.object_count();
            assert_eq!(get_sys_color_brush(&mut env, index).0, brush);
            assert_eq!(env.sys.objects.object_count(), count);
            assert!(
                matches!(env.ext.get::<Win32>().gdi.get(&env.sys.objects, brush),
                Some(GdiObj::Brush {color: actual}) if *actual == color(index).unwrap())
            );
            assert!(matches!(
                crate::gdi32::delete_object_value(&mut env, brush),
                adv_machine::Outcome::Return(1)
            ));
            assert!(
                env.ext
                    .get::<Win32>()
                    .gdi
                    .get(&env.sys.objects, brush)
                    .is_some()
            );
        }
        assert_eq!(get_sys_color(&mut env, 15), rgb(212, 208, 200));
        assert_ne!(get_sys_color(&mut env, 5), get_sys_color(&mut env, 8));
        for index in [-1, 25, 31, i32::MAX] {
            assert_eq!(get_sys_color(&mut env, index), 0);
            assert_eq!(get_sys_color_brush(&mut env, index).0, 0);
        }
    }
}
