//! NT open uses the same checked native file/directory semantic core as create.
use adv_abi::{GuestPtr, Registry};
use adv_machine::{Env, Outcome};

fn open_file(env: &mut Env<'_>) -> Outcome {
    let args: [u32; 6] = std::array::from_fn(|index| env.arg(index as u32));
    Outcome::Return(super::file::create(
        env,
        GuestPtr::new(args[0]),
        args[1],
        GuestPtr::new(args[2]),
        GuestPtr::new(args[3]),
        GuestPtr::new(0),
        0,
        args[4],
        1,
        args[5],
        GuestPtr::new(0),
        0,
    ) as u32)
}

pub(super) fn register(reg: &mut Registry) {
    crate::stdcall(reg, "ntdll.dll", "NtOpenFile", 6, open_file);
    crate::stdcall(reg, "ntdll.dll", "ZwOpenFile", 6, open_file);
}
