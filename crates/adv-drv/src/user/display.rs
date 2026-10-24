//! The display the driver reports: one adapter and monitor, at a fixed
//! 1920x1080 desktop, with the bounded mode list the old stack's DirectDraw
//! offers. Fronts change what is presented; the desktop never changes.
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::layout::PAGE;

use super::MODULE;

/// The desktop's size, as the old stack's screen.
pub const DESKTOP: (u32, u32) = (1920, 1080);
const SIZES: [(u32, u32); 6] = [
    (640, 480),
    (800, 600),
    (1024, 768),
    (1280, 720),
    (1600, 900),
    (1920, 1080),
];
const DEPTHS: [u32; 2] = [32, 16];

/// `sizeof(DEVMODEW)`, and its fields the modes set.
const DEVMODE: u32 = 220;
const DM_SIZE: u32 = 68;
const DM_FIELDS: u32 = 72;
const DM_BITS_PER_PEL: u32 = 168;
const DM_PELS_WIDTH: u32 = 172;
const DM_PELS_HEIGHT: u32 = 176;
const DM_DISPLAY_FREQUENCY: u32 = 184;
const FIELDS: u32 = 0x80 | 0x4_0000 | 0x8_0000 | 0x10_0000 | 0x20_0000 | 0x40_0000;
const DM_POSITION: u32 = 0x20;
/// ATTACHED_TO_DESKTOP, PRIMARY_DEVICE and VGA_COMPATIBLE.
const SOURCE_FLAGS: u32 = 0x15;
const DPI: u32 = 96;

/// Where the page that `UpdateDisplayDevices` hands win32u lays out each
/// part: `struct pci_id`, the source name, `struct gdi_monitor`, then the
/// current mode followed by the mode list.
const PCI_ID: u32 = 0;
const NAME: u32 = 8;
const MONITOR: u32 = 16;
const CURRENT: u32 = 64;
const MODES: u32 = CURRENT + DEVMODE;

/// The guest page that describes the display.
struct Display(u32);

pub(super) fn register(registry: &mut Registry) {
    registry.add(
        MODULE,
        "UpdateDisplayDevices",
        Conv::Cdecl { argc: 2 },
        update_display_devices,
    );
}

fn mode(width: u32, height: u32, depth: u32) -> [u8; DEVMODE as usize] {
    let mut mode = [0; DEVMODE as usize];
    for (offset, value) in [
        (DM_FIELDS, FIELDS),
        (DM_BITS_PER_PEL, depth),
        (DM_PELS_WIDTH, width),
        (DM_PELS_HEIGHT, height),
        (DM_DISPLAY_FREQUENCY, 60),
    ] {
        mode[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    }
    mode[DM_SIZE as usize..DM_SIZE as usize + 2].copy_from_slice(&(DEVMODE as u16).to_le_bytes());
    mode
}

/// Write the display's page.
pub(super) fn install(
    machine: &mut adv_machine::Machine<impl adv_cpu::backend::Backend>,
) -> Result<(), String> {
    let page = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0u8; PAGE as usize];
    bytes[NAME as usize..NAME as usize + 8].copy_from_slice(b"Default\0");
    let (width, height) = DESKTOP;
    for (index, side) in [0, 0, width, height, 0, 0, width, height]
        .into_iter()
        .enumerate()
    {
        let at = MONITOR as usize + 4 * index;
        bytes[at..at + 4].copy_from_slice(&side.to_le_bytes());
    }
    let mut current = mode(width, height, DEPTHS[0]);
    current[DM_FIELDS as usize..DM_FIELDS as usize + 4]
        .copy_from_slice(&(FIELDS | DM_POSITION).to_le_bytes());
    bytes[CURRENT as usize..MODES as usize].copy_from_slice(&current);
    let modes = DEPTHS
        .into_iter()
        .flat_map(|depth| SIZES.map(|(width, height)| mode(width, height, depth)));
    for (index, mode) in modes.enumerate() {
        let at = (MODES + DEVMODE * index as u32) as usize;
        bytes[at..at + DEVMODE as usize].copy_from_slice(&mode);
    }
    machine.cpu.write(page, &bytes).map_err(|e| e.to_string())?;
    machine.ext.insert(Display(page));
    Ok(())
}

/// The virtual screen's rectangle, in guest memory.
pub(super) fn screen(env: &Env<'_>) -> u32 {
    // The monitor's rectangle comes first in `struct gdi_monitor`.
    env.ext.get::<Display>().0 + MONITOR
}

/// UpdateDisplayDevices(*manager, param): the adapter, its source, the
/// monitor and the modes, through the manager's callbacks.
fn update_display_devices(env: &mut Env<'_>) -> Outcome {
    let (manager, param) = (env.arg(0), env.arg(1));
    let page = env.ext.get::<Display>().0;
    let callbacks = (0..4)
        .map(|index| env.cpu.read_u32(manager + 4 * index))
        .collect();
    let Ok::<Vec<u32>, _>(callbacks) = callbacks else {
        return Outcome::Fail(format!("{MODULE}: unreadable device manager"));
    };
    let &[add_gpu, add_source, add_monitor, add_modes] = callbacks.as_slice() else {
        unreachable!()
    };
    let count = (SIZES.len() * DEPTHS.len()) as u32;
    Outcome::call_aligned(env, add_gpu, [0, page + PCI_ID, 0, param], move |env, _| {
        let source = [page + NAME, SOURCE_FLAGS, DPI, param];
        Outcome::call_aligned(env, add_source, source, move |env, _| {
            Outcome::call_aligned(env, add_monitor, [page + MONITOR, param], move |env, _| {
                let modes = [page + CURRENT, count, page + MODES, param];
                Outcome::call_aligned(env, add_modes, modes, |_, _| Outcome::Return(0))
            })
        })
    })
}
