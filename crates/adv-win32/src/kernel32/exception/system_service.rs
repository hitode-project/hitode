//! Native x86 numeric services selected by the Windows personality's build.
//! Published Win7 SP1 identities select existing owners, independently of
//! private HLE transport tokens or the current host's service table.
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{
    Env, Outcome,
    exception::Checkpoint,
    layout::{THUNK_BASE, THUNK_STRIDE},
};

mod win7;

/// Publish callable native service veneers only for existing implementations.
/// They execute the native x86 EAX/EDX ABI directly and need no shared-data page.
/// Unimplemented named exports retain the existing strict/discovery policy.
pub(crate) fn install<B: adv_cpu::backend::Backend>(
    m: &mut adv_machine::Machine<B>,
) -> Result<(), adv_cpu::backend::MemFault> {
    let profile = m.ext.get::<crate::Win32>().cfg.personality;
    if (profile.major, profile.minor, profile.build) != (6, 1, 7601) {
        return Ok(());
    }
    let entries: Vec<_> = win7::WIN7_SP1
        .iter()
        .enumerate()
        .filter_map(|(id, &name)| {
            let resolver = m.sys.thunks.resolver();
            if !resolver.implemented("ntdll.dll", name) {
                return None;
            }
            let function = resolver.host_fn("ntdll.dll", name)?;
            let adv_machine::Conv::Stdcall { argc } = function.conv else {
                return None;
            };
            Some((id as u32, name, argc))
        })
        .collect();
    for (id, name, argc) in entries {
        let alias = format!("Zw{}", &name[2..]);
        let mut code = adv_cpu::backend::asm::Asm::new(0);
        code.bytes.push(0xb8);
        code.bytes.extend(id.to_le_bytes());
        code.bytes.extend([0x8d, 0x54, 0x24, 0x04]);
        code.trap().ret_n(u16::from(argc) * 4);
        for export in [name, &alias] {
            if !m.sys.thunks.resolver().implemented("ntdll.dll", export) {
                continue;
            }
            let entry = m.sys.thunks.publish(&mut m.cpu, "ntdll.dll", export)?;
            let index = (entry - THUNK_BASE) / THUNK_STRIDE;
            m.sys.thunks.set_export_entry(index, code.bytes.clone());
        }
    }
    Ok(())
}

const INVALID_SYSTEM_SERVICE: u32 = 0xc000_001c;
const NOT_IMPLEMENTED: u32 = 0xc000_0002;
const ACCESS_VIOLATION: u32 = 0xc000_0005;

#[derive(Debug, PartialEq, Eq)]
struct Number {
    table: usize,
    index: usize,
}
impl Number {
    fn decode(raw: u32) -> Self {
        Self {
            table: ((raw >> 12) & 1) as usize,
            index: (raw & 0xfff) as usize,
        }
    }
}

fn lookup(raw: u32) -> Option<&'static str> {
    let number = Number::decode(raw);
    // Preserve native x86 selector decoding. GUI numeric services remain absent.
    (number.table == 0)
        .then(|| win7::WIN7_SP1.get(number.index).copied())
        .flatten()
}

pub(super) fn dispatch(env: &mut Env<'_>, mut checkpoint: Checkpoint) -> Outcome {
    let profile = env.ext.get::<crate::Win32>().cfg.personality;
    if (profile.major, profile.minor, profile.build) != (6, 1, 7601) {
        adv_abi::warn_unavailable(
            env,
            "native x86 INT 2E",
            "numeric service directory is unavailable for this personality build",
        );
        checkpoint.state.regs[0] = INVALID_SYSTEM_SERVICE;
        return Outcome::ResumeExecution(checkpoint);
    }
    let Some(name) = lookup(checkpoint.state.regs[0]) else {
        adv_abi::warn_unavailable(
            env,
            "native x86 INT 2E",
            "numeric service is unavailable in the selected directory",
        );
        checkpoint.state.regs[0] = INVALID_SYSTEM_SERVICE;
        return Outcome::ResumeExecution(checkpoint);
    };
    let resolver = env.sys.thunks.resolver();
    if !resolver.implemented("ntdll.dll", name) {
        adv_abi::warn_unavailable(
            env,
            "native x86 INT 2E",
            &format!("{name} implementation is unavailable"),
        );
        checkpoint.state.regs[0] = NOT_IMPLEMENTED;
        return Outcome::ResumeExecution(checkpoint);
    }
    let Some(function) = resolver.host_fn("ntdll.dll", name) else {
        return Outcome::Fail("implemented numeric service has no host binding".into());
    };
    let adv_machine::Conv::Stdcall { argc } = function.conv else {
        return Outcome::Fail("native x86 numeric service requires a fixed stdcall binding".into());
    };
    let pointer = checkpoint.state.regs[2];
    let length = u32::from(argc) * 4;
    // Empty parameters never inspect EDX. Snapshot all inputs before an owner
    // or callback can overwrite memory overlapping its argument source.
    if length != 0 && adv_abi::guest::check_range(env.cpu, pointer, length, Prot::READ).is_err() {
        checkpoint.state.regs[0] = ACCESS_VIOLATION;
        return Outcome::ResumeExecution(checkpoint);
    }
    let mut args = Vec::with_capacity(usize::from(argc));
    for i in 0..u32::from(argc) {
        let Ok(value) = env.cpu.read_u32(pointer + i * 4) else {
            checkpoint.state.regs[0] = ACCESS_VIOLATION;
            return Outcome::ResumeExecution(checkpoint);
        };
        args.push(value);
    }
    let entry = match env.sys.thunks.resolve(env.cpu, "ntdll.dll", name) {
        Ok(entry) => entry,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    let binding = (entry - THUNK_BASE) / THUNK_STRIDE;
    env.invoke_host_u32(binding, args, move |_, result| {
        checkpoint.state.regs[0] = result.value();
        Outcome::ResumeExecution(checkpoint)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_directory_entries_have_fixed_native_signatures() {
        let mut registry = adv_abi::Registry::default();
        crate::register(&mut registry);
        for &name in win7::WIN7_SP1 {
            if registry.is_implemented("ntdll.dll", name) {
                assert!(
                    matches!(
                        registry.get("ntdll.dll", name).unwrap().conv,
                        adv_machine::Conv::Stdcall { .. }
                    ),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn build_numbers_are_separate_from_transport_and_gui_selectors() {
        assert_eq!(win7::WIN7_SP1.len(), 401);
        for (index, name) in [
            (50, "NtClose"),
            (60, "NtContinue"),
            (64, "NtCreateEvent"),
            (271, "NtRaiseException"),
            (300, "NtResetEvent"),
            (323, "NtSetEvent"),
            (335, "NtSetInformationThread"),
            (372, "NtTestAlert"),
            (391, "NtWaitForSingleObject"),
        ] {
            for high in [0, 0x2000, 0x8000_0000, 0xffff_e000] {
                assert_eq!(lookup(high | index), Some(name));
                assert_eq!(lookup(high | 0x1000 | index), None);
            }
        }
        for index in [401, 0x7ff, 0xfff] {
            assert_eq!(lookup(index), None);
        }
    }
}
