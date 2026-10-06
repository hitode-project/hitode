//! EUDC session font linking is not exposed by the virtual font frontend.
use adv_abi::{Env, Registry, win32};

#[win32(gdi32, name = "EnableEUDC", level = safe_fail)]
fn enable(env: &mut Env<'_>, _enable: i32) -> i32 {
    adv_abi::warn_unavailable(
        env,
        "EnableEUDC",
        "EUDC session font linking is unavailable",
    );
    0
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(enable_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_abi::{ApiDiagnostics, ImplementationLevel};
    use adv_cpu::{
        JitCpu,
        backend::{Backend, GuestExt, Prot, asm::Asm},
    };
    use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
    use adv_unicorn::UnicornCpu;

    fn run<B: Backend>(cpu: B) {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        assert!(registry.signature_conflicts().is_empty());
        assert_eq!(
            registry.level("gdi32.dll", "EnableEUDC"),
            Some(ImplementationLevel::SafeFail)
        );
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let code = 0x400000;
        let data = 0x410000;
        m.cpu.map(code, 4096, Prot::RX).unwrap();
        m.cpu.map(data, 4096, Prot::RW).unwrap();
        m.spawn("EUDC caller", code, &[], 65536).unwrap();
        let api = m.resolve_import("gdi32.dll", "EnableEUDC").unwrap();
        let done = m.resolve_import("test", "done").unwrap();
        let mut asm = Asm::new(code);
        asm.bytes.extend_from_slice(&[0x89, 0xe0]);
        asm.mov_mem_eax(data);
        for (index, flag) in [0, 1, u32::MAX].into_iter().enumerate() {
            asm.push_imm(flag)
                .call(api)
                .mov_mem_eax(data + 8 + index as u32 * 4);
        }
        asm.bytes.extend_from_slice(&[0x89, 0xe0]);
        asm.mov_mem_eax(data + 4).call(done).hlt();
        m.cpu.write(code, &asm.bytes).unwrap();
        let ctx_teb = m.sys.context(0).teb;
        m.cpu
            .write_u32(ctx_teb + adv_machine::layout::teb::LAST_ERROR, 0x777)
            .unwrap();
        assert!(matches!(m.run(), RunResult::Exited(0)));
        assert_eq!(
            m.cpu.read_u32(data).unwrap(),
            m.cpu.read_u32(data + 4).unwrap()
        );
        for index in 0..3 {
            assert_eq!(m.cpu.read_u32(data + 8 + index * 4).unwrap(), 0);
        }
        assert_eq!(
            m.cpu
                .read_u32(ctx_teb + adv_machine::layout::teb::LAST_ERROR)
                .unwrap(),
            0x777
        );
        let diagnostics = m.ext.get::<ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(api, _)| api == "EnableEUDC")
        );
    }

    #[test]
    fn eudc_failure_uses_verified_bool_abi_and_does_not_change_desktop_state() {
        run(JitCpu::new(PARK_ADDR, false).unwrap());
        run(JitCpu::new(PARK_ADDR, true).unwrap());
        run(UnicornCpu::new(PARK_ADDR).unwrap());
    }
}
