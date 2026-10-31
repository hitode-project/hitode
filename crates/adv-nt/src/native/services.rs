//! Service bodies borrow shared host storage, never the machine or extensions.

use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};

fn complete<M: Mem>(mem: &M, value: u32) -> Result<Ret, Stop> {
    // SAFETY: body services retain their shared machine allocations.
    unsafe { adv_machine::service::work(mem.services()) };
    Ok(Ret::from(value))
}

adv_cpu::body!(pub(super) NT_COUNTER = nt_counter, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
adv_cpu::body!(pub(super) COUNTER = counter, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);

fn ticks<M: Mem>(mem: &M) -> u64 {
    let services = mem.services();
    // SAFETY: the machine retains its shared clock throughout execution.
    unsafe { adv_machine::Clock::body_counter(services.clock, services.epoch_us) }
}

fn output<M: Mem>(mem: &mut M, at: u32) -> Result<*mut u8, Stop> {
    mem.output(at, 8)
}

// Windows 11 WoW64 probes both outputs before writing either; unaligned
// (not four-byte aligned) outputs return STATUS_DATATYPE_MISALIGNMENT.
fn nt_counter<M: Mem>(mem: &mut M, [at, frequency, ..]: [u32; 7]) -> Result<Ret, Stop> {
    if at & 3 != 0 {
        return complete(mem, 0x8000_0002u32);
    }
    let outputs = (|| {
        let counter = output(mem, at)?;
        let frequency = if frequency == 0 {
            None
        } else {
            if frequency & 3 != 0 {
                return Ok(None);
            }
            Some(output(mem, frequency)?)
        };
        Ok(Some((counter, frequency)))
    })();
    let (counter, frequency) = match outputs {
        Ok(Some(outputs)) => outputs,
        Ok(None) => return complete(mem, 0x8000_0002u32),
        Err(Stop::Fault { .. }) => return complete(mem, crate::status::ACCESS_VIOLATION),
        Err(stop) => return Err(stop),
    };
    let counter_value = ticks(mem);
    // SAFETY: both complete outputs were checked, including overlap.
    unsafe {
        counter.cast::<u64>().write_unaligned(counter_value.to_le());
        if let Some(frequency) = frequency {
            frequency
                .cast::<u64>()
                .write_unaligned(crate::system::TICKS_PER_SECOND.to_le());
        }
    }
    mem.commit()?;
    complete(mem, 0u32)
}

// Windows 11's Win32 and Rtl entries return TRUE without changing last error;
// inaccessible outputs raise access violations, while unaligned stores work.
fn counter<M: Mem>(mem: &mut M, [at, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let output = output(mem, at)?;
    let value = ticks(mem);
    // SAFETY: the complete output was checked.
    unsafe { output.cast::<u64>().write_unaligned(value.to_le()) };
    mem.commit()?;
    complete(mem, 1u32)
}

adv_cpu::body!(GET_FILE_SIZE = get_file_size, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
adv_cpu::body!(GET_FILE_SIZE_EX = get_file_size_ex, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
adv_cpu::body!(SET_FILE_POINTER = set_file_pointer, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
adv_cpu::body!(SET_FILE_POINTER_EX = set_file_pointer_ex, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3), Input::Stack(4)], outputs: EAX, pop: 20);

fn file<M: Mem>(mem: &M, handle: u32) -> Result<crate::file::state::Entry, Stop> {
    let state = mem.services().state.cast::<crate::file::state::States>();
    if state.is_null() {
        return Err(Stop::Guest);
    }
    // SAFETY: Link retains this shared allocation; all access uses its RefCell.
    unsafe { &*state }.get(handle).ok_or(Stop::Guest)
}

fn error<M: Mem>(mem: &mut M, error: u32, value: u32) -> Result<Ret, Stop> {
    let last_error = mem.output(mem.services().teb + 0x34, 4)?;
    // SAFETY: checked thread-local last-error output.
    unsafe { last_error.cast::<u32>().write_unaligned(error.to_le()) };
    mem.commit()?;
    complete(mem, value)
}

fn dos_error(status: u32) -> u32 {
    match status {
        0xc000_0043 => 32,
        0x8000_0011 => 170,
        0xc000_007f => 112,
        0xc000_0022 => 5,
        0xc000_003a => 3,
        0xc000_0034 => 2,
        0xc000_011f => 4,
        0xc000_0101 => 145,
        0xc000_0280 => 1921,
        0xc000_0010 | 0xc000_00af | 0xc000_0002 => 1,
        0xc000_000d => 87,
        0xc000_00b0 => 233,
        0xc000_00a3 => 21,
        0xc000_000e => 433,
        0xc000_00bb => 50,
        0xc000_0005 => 998,
        0xc000_00b5 => 121,
        0xc000_0008 | 0xc000_0024 => 6,
        _ => 31,
    }
}

fn get_file_size<M: Mem>(mem: &mut M, [handle, high, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let file = file(mem, handle)?;
    let size = match file.size() {
        Ok(size) => size,
        Err(status) => return error(mem, dos_error(status), u32::MAX),
    };
    let high = if high == 0 {
        None
    } else {
        Some(mem.output(high, 4)?)
    };
    // Check last error before publishing either output.
    let last = if size as u32 == u32::MAX {
        Some(mem.output(mem.services().teb + 0x34, 4)?)
    } else {
        None
    };
    // SAFETY: checked outputs, ordered like GetFileSize.
    unsafe {
        if let Some(high) = high {
            high.cast::<u32>()
                .write_unaligned(((size >> 32) as u32).to_le());
        }
        if let Some(last) = last {
            last.cast::<u32>().write_unaligned(0);
        }
    }
    mem.commit()?;
    complete(mem, size as u32)
}

fn get_file_size_ex<M: Mem>(mem: &mut M, [handle, at, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let file = file(mem, handle)?;
    let size = match file.size() {
        Ok(size) => size,
        Err(status) => return error(mem, dos_error(status), 0),
    };
    let out = mem.output(at, 8)?;
    // SAFETY: checked output; Win32's store faults on an inaccessible pointer.
    unsafe { out.cast::<u64>().write_unaligned(size.to_le()) };
    mem.commit()?;
    complete(mem, 1u32)
}

fn position(file: &crate::file::state::Entry, distance: i64, method: u32) -> Result<i64, u32> {
    if file.kind == 3 {
        return Err(1);
    }
    let base = match method {
        0 => 0,
        1 => file.position.get(),
        2 => file.size().map_err(dos_error)? as i64,
        _ => return Err(87),
    };
    let position = base.wrapping_add(distance);
    if position < 0 { Err(131) } else { Ok(position) }
}

fn set_file_pointer_ex<M: Mem>(
    mem: &mut M,
    [handle, low, high, out, method, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    if method > 2 {
        return error(mem, 87, 0);
    }
    let distance = (u64::from(low) | u64::from(high) << 32) as i64;
    if method == 0 && distance < 0 {
        return error(mem, 131, 0);
    }
    let file = file(mem, handle)?;
    let at = match position(&file, distance, method) {
        Ok(at) => at,
        Err(code) => return error(mem, code, 0),
    };
    let output = if out == 0 {
        Ok(None)
    } else {
        mem.output(out, 8).map(Some)
    };
    if output == Err(Stop::Slow) {
        return Err(Stop::Slow);
    }
    file.position.set(at);
    if let Some(output) = output? {
        // SAFETY: the output was checked before changing host state.
        unsafe { output.cast::<i64>().write_unaligned(at.to_le()) };
        mem.commit()?;
    }
    complete(mem, 1u32)
}

fn set_file_pointer<M: Mem>(
    mem: &mut M,
    [handle, low, high, method, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let distance = if high == 0 {
        i64::from(low as i32)
    } else {
        (u64::from(low) | u64::from(mem.u32(high)?) << 32) as i64
    };
    if method > 2 {
        return error(mem, 87, u32::MAX);
    }
    if method == 0 && distance < 0 {
        return error(mem, 131, u32::MAX);
    }
    let file = file(mem, handle)?;
    let at = match position(&file, distance, method) {
        Ok(at) => at,
        Err(code) => return error(mem, code, u32::MAX),
    };
    let output = if high == 0 {
        Ok(None)
    } else {
        mem.output(high, 4).map(Some)
    };
    if output == Err(Stop::Slow) {
        return Err(Stop::Slow);
    }
    let last = if at as u32 == u32::MAX {
        Some(mem.output(mem.services().teb + 0x34, 4)?)
    } else {
        None
    };
    file.position.set(at);
    if let Some(output) = output? {
        // SAFETY: checked high-word output.
        unsafe {
            output
                .cast::<u32>()
                .write_unaligned(((at as u64 >> 32) as u32).to_le())
        };
    }
    if let Some(last) = last {
        // SAFETY: checked last-error output.
        unsafe { last.cast::<u32>().write_unaligned(0) };
    }
    mem.commit()?;
    complete(mem, at as u32)
}

pub(super) fn bodies() -> impl Iterator<Item = super::Body> {
    let files = [
        ("kernelbase.dll!GetFileSize", 2, GET_FILE_SIZE),
        ("kernelbase.dll!GetFileSizeEx", 2, GET_FILE_SIZE_EX),
        ("kernelbase.dll!SetFilePointer", 4, SET_FILE_POINTER),
        ("kernelbase.dll!SetFilePointerEx", 5, SET_FILE_POINTER_EX),
    ]
    .into_iter()
    .map(|(name, argc, direct)| super::Body {
        name: name.into(),
        conv: adv_machine::Conv::Stdcall { argc },
        body: None,
        handler: super::resume_original,
        direct: Some(direct),
    });
    #[cfg(any(target_vendor = "apple", target_os = "android"))]
    let files = files.chain(objects::bodies());
    files
}

#[cfg(any(target_vendor = "apple", target_os = "android"))]
mod objects {
    use super::*;
    use crate::ntsync;

    adv_cpu::body!(SET_EVENT = set_event, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(RESET_EVENT = reset_event, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(RELEASE_SEMAPHORE = release_semaphore, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
    adv_cpu::body!(RELEASE_MUTEX = release_mutex, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
    adv_cpu::body!(WAIT_SINGLE = wait_single, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
    adv_cpu::body!(WAIT_SINGLE_EX = wait_single_ex, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
    adv_cpu::body!(WAIT_MULTIPLE = wait_multiple, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
    adv_cpu::body!(WAIT_MULTIPLE_EX = wait_multiple_ex, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3), Input::Stack(4)], outputs: EAX, pop: 20);

    fn sync<M: Mem>(
        mem: &M,
        handle: u32,
        access: u32,
        kind: i32,
    ) -> Result<Result<i32, u32>, Stop> {
        let tid = mem.services().thread;
        if tid == 0 {
            return Err(Stop::Guest);
        }
        match adv_wineserver::sync_fd(tid, handle, access, kind) {
            Ok(-1) => Err(Stop::Guest),
            result => Ok(result),
        }
    }

    fn signal<M: Mem>(
        mem: &mut M,
        handle: u32,
        kind: i32,
        command: u32,
        input: u32,
        previous: u32,
    ) -> Result<Ret, Stop> {
        let fd = match sync(mem, handle, if kind == 3 { 0 } else { 2 }, kind)? {
            Ok(fd) => fd,
            Err(status) => return error(mem, dos_error(status), 0),
        };
        let output = if previous == 0 {
            Ok(None)
        } else {
            mem.output(previous, 4).map(Some)
        };
        if output == Err(Stop::Slow) {
            return Err(Stop::Slow);
        }
        let value = match ntsync::service_signal(fd, command, input).ok_or(Stop::Guest)? {
            Ok(value) => value,
            Err(code) => {
                return error(
                    mem,
                    match code {
                        libc::EOVERFLOW => 298,
                        libc::EPERM => 288,
                        libc::EINVAL => 87,
                        libc::ENOTTY => 6,
                        _ => 6,
                    },
                    0,
                );
            }
        };
        if let Some(output) = output? {
            // SAFETY: checked output, published after the object changes.
            unsafe { output.cast::<u32>().write_unaligned(value[0].to_le()) };
            mem.commit()?;
        }
        complete(mem, 1u32)
    }

    fn set_event<M: Mem>(mem: &mut M, [handle, ..]: [u32; 7]) -> Result<Ret, Stop> {
        signal(mem, handle, 2, ntsync::EVENT_SET, 0, 0)
    }
    fn reset_event<M: Mem>(mem: &mut M, [handle, ..]: [u32; 7]) -> Result<Ret, Stop> {
        signal(mem, handle, 2, ntsync::EVENT_RESET, 0, 0)
    }
    fn release_mutex<M: Mem>(mem: &mut M, [handle, ..]: [u32; 7]) -> Result<Ret, Stop> {
        signal(
            mem,
            handle,
            3,
            ntsync::MUTEX_UNLOCK,
            mem.services().thread,
            0,
        )
    }
    fn release_semaphore<M: Mem>(
        mem: &mut M,
        [handle, count, previous, ..]: [u32; 7],
    ) -> Result<Ret, Stop> {
        if count as i32 <= 0 {
            return error(mem, 87, 0);
        }
        signal(mem, handle, 4, ntsync::SEM_RELEASE, count, previous)
    }

    fn wait<M: Mem>(
        mem: &mut M,
        handles: &[u32],
        all: bool,
        timeout: u32,
        alertable: u32,
    ) -> Result<Ret, Stop> {
        let mut fds = [0; 64];
        for (index, &handle) in handles.iter().enumerate() {
            fds[index] = match sync(mem, handle, 0x10_0000, 0)? {
                Ok(fd) => fd,
                Err(status) => return error(mem, dos_error(status), u32::MAX),
            };
            if all && fds[..index].contains(&fds[index]) {
                return error(mem, 87, u32::MAX);
            }
        }
        if alertable != 0 {
            let alert = match sync(mem, 0, 0, -1)? {
                Ok(fd) => fd,
                Err(_) => return Err(Stop::Guest),
            };
            if ntsync::service_alert(alert, mem.services().thread) {
                return Err(Stop::Guest);
            }
        }
        match ntsync::service_wait(&fds[..handles.len()], mem.services().thread, all) {
            Ok(Some((index, dead))) => complete(mem, index + if dead { 0x80 } else { 0 }),
            Ok(None) if timeout == 0 => complete(mem, 258u32),
            Ok(None) => Err(Stop::Guest),
            Err(_) => error(mem, 6, u32::MAX),
        }
    }

    fn wait_single<M: Mem>(mem: &mut M, [handle, timeout, ..]: [u32; 7]) -> Result<Ret, Stop> {
        wait(mem, &[handle], false, timeout, 0)
    }
    fn wait_single_ex<M: Mem>(
        mem: &mut M,
        [handle, timeout, alertable, ..]: [u32; 7],
    ) -> Result<Ret, Stop> {
        wait(mem, &[handle], false, timeout, alertable)
    }
    fn wait_multiple<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
        let [count, handles, all, timeout, ..] = args;
        wait_multiple_ex(mem, [count, handles, all, timeout, 0, 0, 0])
    }
    fn wait_multiple_ex<M: Mem>(
        mem: &mut M,
        [count, at, all, timeout, alertable, ..]: [u32; 7],
    ) -> Result<Ret, Stop> {
        if count == 0 || count > 64 {
            return error(mem, 87, u32::MAX);
        }
        let input = match mem.input(at, count * 4) {
            Ok(input) => input,
            Err(Stop::Fault { .. }) => return error(mem, 998, u32::MAX),
            Err(stop) => return Err(stop),
        };
        let mut handles = [0; 64];
        for (index, handle) in handles[..count as usize].iter_mut().enumerate() {
            // SAFETY: the entire handle array was checked.
            *handle = u32::from_le(unsafe { input.add(index * 4).cast::<u32>().read_unaligned() });
        }
        wait(
            mem,
            &handles[..count as usize],
            all != 0,
            timeout,
            alertable,
        )
    }

    pub(super) fn bodies() -> impl Iterator<Item = super::super::Body> {
        [
            ("kernelbase.dll!SetEvent", 1, SET_EVENT),
            ("kernelbase.dll!ResetEvent", 1, RESET_EVENT),
            ("kernelbase.dll!ReleaseSemaphore", 3, RELEASE_SEMAPHORE),
            ("kernelbase.dll!ReleaseMutex", 1, RELEASE_MUTEX),
            ("kernelbase.dll!WaitForSingleObject", 2, WAIT_SINGLE),
            ("kernelbase.dll!WaitForSingleObjectEx", 3, WAIT_SINGLE_EX),
            ("kernelbase.dll!WaitForMultipleObjects", 4, WAIT_MULTIPLE),
            (
                "kernelbase.dll!WaitForMultipleObjectsEx",
                5,
                WAIT_MULTIPLE_EX,
            ),
        ]
        .into_iter()
        .map(|(name, argc, direct)| super::super::Body {
            name: name.into(),
            conv: adv_machine::Conv::Stdcall { argc },
            body: None,
            handler: super::super::resume_original,
            direct: Some(direct),
        })
    }
}
