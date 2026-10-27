//! The process surface store, its guest memory adapter and frame accounting.
use adv_cpu::backend::{Backend, Guest, Prot};
use adv_device::surface::{MemoryOperations, MemoryView, SurfaceStore};
use adv_machine::{Env, Machine, Outcome};
use std::borrow::Cow;
use std::time::{Duration, Instant};

pub use adv_device::surface::{
    FrameConsumer, FrameProducer, FrameView, GraphicsStats, PendingFrame, Presenter, Snapshot,
    SurfaceMemory, Surfaces, VideoCursorRegion, frame_mailbox,
};
/// One byte-access adapter, borrowed on the stack by the fronts.
pub(crate) fn guest_memory<'a>(cpu: &'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, addr, len| {
                if let Some(bytes) = cpu.shared_bytes(addr, len) {
                    return Ok(Cow::Borrowed(bytes));
                }
                let mut bytes = vec![0; len];
                cpu.read(addr, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, addr, bytes| cpu.write(addr, bytes).map_err(|e| e.to_string()),
            writable: |cpu, addr, len| {
                let len32 =
                    u32::try_from(len).map_err(|_| "surface memory length overflow".to_owned())?;
                if cpu.accessible(addr, len32, Prot::WRITE) {
                    Ok(())
                } else {
                    Err(format!(
                        "surface memory is not writable at {addr:#x} (+{len})"
                    ))
                }
            },
            copy: |cpu, src, dst, len| cpu.copy_memory(src, dst, len).map_err(|e| e.to_string()),
        },
    )
}
/// The process surface store and frame accounting.
pub(crate) fn install_store<B: Backend>(
    machine: &mut Machine<B>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    adv_device::surface::size(width, height)?;
    if machine.ext.try_get::<SurfaceStore>().is_some() {
        return Err("the process surface store is already installed".into());
    }
    let arena_len = 64 * 1024 * 1024;
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, arena_len, Prot::RW)
        .map_err(|e| e.to_string())?;
    let snapshot = Snapshot {
        width,
        height,
        bgra: vec![0; width as usize * height as usize * 4],
        frames: 0,
        ..Default::default()
    };
    machine.ext.insert(SurfaceStore::new(
        width, height, arena, arena_len, snapshot,
    )?);
    machine.ext.insert(RunControl::default());
    Ok(())
}
#[derive(Default)]
/// Frame accounting of a run, updated by every presentation.
pub(crate) struct RunControl {
    pub(crate) frame_limit: Option<u64>,
    pub(crate) first_frame: Option<Instant>,
    pub(crate) last_frame: Option<Instant>,
    pub(crate) frame_timing: bool,
    pub(crate) frame_intervals: Vec<Duration>,
}

pub(crate) fn frame_completed(env: &mut Env<'_>, serial: u64) -> Option<Outcome> {
    env.sys.frame = serial as u32;
    env.sys.complete_frame();
    let control = env.ext.get_mut::<RunControl>();
    let now = Instant::now();
    control.first_frame.get_or_insert(now);
    if control.frame_timing
        && let Some(last) = control.last_frame
    {
        control.frame_intervals.push(now.duration_since(last));
    }
    control.last_frame = Some(now);
    control
        .frame_limit
        .filter(|&limit| serial >= limit)
        .map(|_| Outcome::ExitProcess(0))
}
