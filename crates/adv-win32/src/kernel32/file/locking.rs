//! Checked fronts for byte-range locks until shared VFS locking is available.
use super::*;
use adv_abi::{Handle, win32};

fn lock_request(env: &mut Env<'_>, handle: u32, api: &str) -> Outcome {
    let Some(file) = env.sys.objects.host::<FileObj>(handle) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    if file.access & (0x8000_0000 | GENERIC_WRITE | 0x1000_0000) == 0 {
        set_last_error(env, err::ACCESS_DENIED);
        return Outcome::Return(0);
    }
    // Byte offsets and lengths are full unsigned 64-bit quantities; ranges
    // beyond EOF are valid. Do not claim success without enforcing ownership,
    // conflicts, exact unlock ranges, and release on the last handle close.
    adv_abi::warn_unavailable(env, api, "shared VFS byte-range locking is unavailable");
    set_last_error(env, 50); // ERROR_NOT_SUPPORTED
    Outcome::Return(0)
}

#[win32(kernel32, name = "LockFile", level = safe_fail)]
fn lock_file(
    env: &mut Env<'_>,
    handle: Handle,
    _offset_low: u32,
    _offset_high: u32,
    _length_low: u32,
    _length_high: u32,
) -> Outcome {
    lock_request(env, handle.0, "kernel32.dll!LockFile")
}

#[win32(kernel32, name = "UnlockFile", level = safe_fail)]
fn unlock_file(
    env: &mut Env<'_>,
    handle: Handle,
    _offset_low: u32,
    _offset_high: u32,
    _length_low: u32,
    _length_high: u32,
) -> Outcome {
    lock_request(env, handle.0, "kernel32.dll!UnlockFile")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(lock_file_ABI);
    registry.register(unlock_file_ABI);
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Prot, Reg},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    use std::path::PathBuf;

    pub(crate) fn fixture(name: &str) -> (Machine<JitCpu>, PathBuf) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/file-contract/tests")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        for (name, argc) in [
            ("LockFile", 5),
            ("UnlockFile", 5),
            ("GetFileInformationByHandle", 2),
        ] {
            assert_eq!(
                registry.get(K32, name).unwrap().conv,
                adv_machine::Conv::Stdcall { argc }
            );
        }
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
        )
        .unwrap();
        machine
            .spawn("file-contract", PARK_ADDR, &[], 65536)
            .unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        machine.cpu.set_reg(Reg::Esp, 0x410800);
        (machine, root)
    }

    pub(crate) fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
    }

    pub(crate) fn returned(outcome: Outcome) -> u32 {
        match outcome {
            Outcome::Return(value) => value,
            _ => panic!("synchronous file contract"),
        }
    }

    pub(crate) fn args(machine: &mut Machine<JitCpu>, values: &[u32]) {
        for (index, value) in values.iter().enumerate() {
            machine
                .cpu
                .write_u32(0x410804 + index as u32 * 4, *value)
                .unwrap();
        }
    }

    #[test]
    fn byte_range_locking_validates_handles_and_fails_without_changing_file_state() {
        let (mut machine, root) = fixture("locking-failure");
        std::fs::write(root.join("game/data.bin"), b"original data").unwrap();
        for (handle, error) in [(INVALID_HANDLE_VALUE, 6), (STD_OUTPUT, 6)] {
            for api in ["kernel32.dll!LockFile", "kernel32.dll!UnlockFile"] {
                assert_eq!(
                    returned(lock_request(&mut env(&mut machine), handle, api)),
                    0
                );
                assert_eq!(crate::util::last_error(&env(&mut machine)), error);
            }
        }
        let denied = open(&mut env(&mut machine), "data.bin", 0, OPEN_EXISTING);
        assert_ne!(denied, INVALID_HANDLE_VALUE);
        assert_eq!(
            returned(lock_request(
                &mut env(&mut machine),
                denied,
                "kernel32.dll!LockFile"
            )),
            0
        );
        assert_eq!(
            crate::util::last_error(&env(&mut machine)),
            err::ACCESS_DENIED
        );
        let file = open(
            &mut env(&mut machine),
            "data.bin",
            0x8000_0000,
            OPEN_EXISTING,
        );
        assert_ne!(file, INVALID_HANDLE_VALUE);
        let count = machine.sys.objects.object_count();
        for handler in [lock_file_ABI.handler, unlock_file_ABI.handler] {
            args(
                &mut machine,
                &[file, u32::MAX, u32::MAX, u32::MAX, u32::MAX],
            );
            assert_eq!(returned(handler(&mut env(&mut machine))), 0);
            assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
            assert_eq!(machine.sys.objects.object_count(), count);
            assert_eq!(
                machine
                    .sys
                    .objects
                    .host::<FileObj>(file)
                    .unwrap()
                    .file
                    .stream_position()
                    .unwrap(),
                0
            );
        }
        args(&mut machine, &[file, 0x410100, 13, 0x410180, 0]);
        assert_eq!(returned(read_file(&mut env(&mut machine))), 1);
        let mut bytes = [0; 13];
        machine.cpu.read(0x410100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"original data");
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
