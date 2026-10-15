//! Desktop composition is unavailable in the classic guest window profile.
use adv_abi::{Api, Conv, Env, ImplementationLevel, Outcome, Registry};

fn set_attribute(env: &mut Env<'_>) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        "dwmapi.dll!DwmSetWindowAttribute",
        "desktop composition and iconic window previews are unavailable",
    );
    // Documented Windows 7 composition-disabled HRESULT; no attribute is stored.
    Outcome::Return(0x80263001)
}

pub(super) fn register(registry: &mut Registry) {
    // The SDK prototype is HRESULT WINAPI(HWND, DWORD, LPCVOID, DWORD).
    // This DLL is outside the pinned metadata projection's module scope.
    registry.register(Api {
        module: "dwmapi.dll",
        name: "DwmSetWindowAttribute",
        conv: Conv::Stdcall { argc: 4 },
        env_leaf: None,
        handler: set_attribute,
        level: ImplementationLevel::SafeFail,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot, Reg},
    };
    use adv_machine::{
        Clock, Machine,
        layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
    };

    #[test]
    fn disabled_composition_export_returns_hresult_without_changing_attributes_or_last_error() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert!(registry.dynamic_available("dwmapi.dll", "DwmSetWindowAttribute"));
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.spawn("test", PARK_ADDR, &[], 65536).unwrap();
        m.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        m.cpu.set_reg(Reg::Esp, 0x410000);
        for (i, value) in [0, 0, 7, 0x410100, 4].into_iter().enumerate() {
            m.cpu.write_u32(0x410000 + i as u32 * 4, value).unwrap();
        }
        m.cpu.write_u32(0x410100, 0x12345678).unwrap();
        let thunk = (m
            .resolve_import("dwmapi.dll", "DwmSetWindowAttribute")
            .unwrap()
            - THUNK_BASE)
            / THUNK_STRIDE;
        let mut env = Env {
            context: 0,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        };
        crate::util::set_last_error(&mut env, 0x1357);
        assert!(matches!(
            set_attribute(&mut env),
            Outcome::Return(0x80263001)
        ));
        assert_eq!(crate::util::last_error(&env), 0x1357);
        assert_eq!(env.cpu.read_u32(0x410100).unwrap(), 0x12345678);
    }
}
