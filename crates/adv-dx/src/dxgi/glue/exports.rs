//! Windows 7 export signatures with the current runtime's DXGI behavior.
use super::*;
use crate::interop::ddi::register_proc;
use adv_cpu::backend::{Fault, FaultKind, Reg};

pub(super) fn register(registry: &mut Registry) {
    crate::interop::ddi::register(registry, DLL);
    for (name, argc) in [
        ("DXGID3D10CreateDevice", 6),
        ("DXGID3D10CreateLayeredDevice", 5),
        ("DXGID3D10RegisterLayers", 2),
    ] {
        register_proc(registry, DLL, name, argc, |_| Outcome::Return(0x80004001));
    }
    register_proc(registry, DLL, "DXGID3D10GetLayeredDeviceSize", 2, zero);
    register_proc(registry, DLL, "DXGIDumpJournal", 1, dump_journal);
    register_proc(
        registry,
        DLL,
        "DXGIReportAdapterConfiguration",
        1,
        report_adapter,
    );
}

fn report_adapter(env: &mut Env<'_>) -> Outcome {
    let error = if env.arg(0) == 0 {
        Error::InvalidArgument
    } else {
        Error::NotFound
    };
    Outcome::Return(error.hresult())
}

fn dump_journal(env: &mut Env<'_>) -> Outcome {
    let callback = env.arg(0);
    if callback == 0 {
        return Outcome::Raise(Fault {
            kind: FaultKind::Read(0),
            pc: env.cpu.reg(Reg::Eip),
        });
    }
    journal_entry(env, callback, 64)
}

fn journal_entry(env: &mut Env<'_>, callback: u32, remaining: u32) -> Outcome {
    if remaining == 0 {
        return Outcome::Return(0);
    }
    const LINE: &[u8] = b"DXGI Error 00000000: (0@0) at 00000000 - \r\n\0";
    let scratch = env
        .cpu
        .reg(Reg::Esp)
        .wrapping_sub(LINE.len().next_multiple_of(4) as u32);
    if env.cpu.write(scratch, LINE).is_err() {
        return Outcome::Raise(Fault {
            kind: FaultKind::Write(scratch),
            pc: env.cpu.reg(Reg::Eip),
        });
    }
    Outcome::call_on_stack(callback, [scratch], scratch, move |env, _| {
        journal_entry(env, callback, remaining - 1)
    })
}
