//! Registration of the existing optional compatibility personality, by DLL.
use adv_abi::Registry;

pub fn register(registry: &mut Registry) {
    crate::msacm32::register(registry);
    crate::winmm::mmio::register(registry);
    crate::avifil32::register(registry);
    crate::user32::compat::register(registry);
    crate::imm32::register_compat(registry);
    crate::ole32::register(registry);
    crate::kernel32::unsupported_process::register(registry);
}
