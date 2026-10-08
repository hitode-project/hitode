//! Guest process error-mode state, shared by all of its guest threads.
use adv_abi::{Registry, win32};
use adv_machine::Env;

const SEM_NOALIGNMENTFAULTEXCEPT: u32 = 0x0004;

#[derive(Default)]
struct ErrorMode(u32);

impl ErrorMode {
    fn replace(&mut self, mode: u32) -> u32 {
        let previous = self.0;
        // Windows keeps alignment-fault suppression once enabled. Preserve all
        // other input bits as native SetErrorMode does, without filtering them.
        self.0 = mode | (previous & SEM_NOALIGNMENTFAULTEXCEPT);
        previous
    }
}

#[win32(kernel32, name = "GetErrorMode", level = state)]
fn get_error_mode(env: &mut Env<'_>) -> u32 {
    env.ext.get_or_default::<ErrorMode>().0
}

#[win32(kernel32, name = "SetErrorMode", level = state)]
fn set_error_mode(env: &mut Env<'_>, mode: u32) -> u32 {
    env.ext.get_or_default::<ErrorMode>().replace(mode)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_error_mode_ABI);
    registry.register(set_error_mode_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_returns_previous_and_clears_nonsticky_flags() {
        let mut mode = ErrorMode::default();
        assert_eq!(mode.replace(0x8003), 0);
        assert_eq!(mode.replace(2), 0x8003);
        assert_eq!(mode.replace(0), 2);
        assert_eq!(mode.0, 0);
    }

    #[test]
    fn alignment_flag_is_sticky_and_unknown_bits_roundtrip() {
        let mut mode = ErrorMode::default();
        assert_eq!(mode.replace(u32::MAX), 0);
        assert_eq!(mode.0, u32::MAX);
        assert_eq!(mode.replace(0), u32::MAX);
        assert_eq!(mode.0, SEM_NOALIGNMENTFAULTEXCEPT);
        assert_eq!(mode.replace(0x8000), SEM_NOALIGNMENTFAULTEXCEPT);
        assert_eq!(mode.0, 0x8004);
    }
}
