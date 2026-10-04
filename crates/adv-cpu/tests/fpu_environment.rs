use adv_cpu::backend::{FpuEnvironment, FpuEnvironmentError, Guest, MemFault, Prot, Reg};
use adv_cpu::{JitCpu, X87Register};

#[test]
fn control_masks_recompute_pending_without_losing_exception_or_stack_state() {
    let mut cpu = JitCpu::new(0x7e100000, false).unwrap();
    let state = &mut cpu.executor_mut().state;
    state
        .x87
        .push(X87Register::from_parts(0xa000_0000_0000_0000, 0x3fff));
    state.x87.status |= 0x401;
    state.x87.ip = 0x123456;
    state.x87.dp = 0xabcdef;
    let original = state.x87.clone();
    cpu.set_fpu_environment(FpuEnvironment {
        x87_control: 0x027e,
        mxcsr: 0x1fa3,
    })
    .unwrap();
    let mut expected = original.clone();
    expected.control = 0x027e;
    expected.status |= 0x8080;
    assert_eq!(
        cpu.executor().state.x87,
        expected,
        "unmasking exposes pending invalid exception"
    );
    cpu.set_fpu_environment(FpuEnvironment {
        x87_control: 0x027f,
        mxcsr: 0x1fa3,
    })
    .unwrap();
    assert_eq!(
        cpu.executor().state.x87,
        original,
        "masking does not clear sticky flags"
    );
    assert_eq!(cpu.executor().state.mxcsr, 0x1fa3);
}

#[test]
fn environment_read_materializes_mmx_and_invalid_write_does_not_materialize_it() {
    let mut cpu = JitCpu::new(0x7e100000, false).unwrap();
    let state = &mut cpu.executor_mut().state;
    state.mmx[3] = 0x1234_5678_90ab_cdef;
    state.mmx_mode = 1 | 4 | (1 << 11);
    let before = state.clone();
    assert_eq!(
        cpu.set_fpu_environment(FpuEnvironment {
            x87_control: 0x007f,
            mxcsr: 0x20000
        }),
        Err(FpuEnvironmentError::InvalidMxcsr(0x20000))
    );
    assert_eq!(cpu.executor().state, before);
    cpu.fpu_environment().unwrap();
    let state = &cpu.executor().state;
    assert_eq!(state.mmx_mode, 0);
    assert_eq!(
        state.x87.regs[3],
        X87Register::from_parts(0x1234_5678_90ab_cdef, 0xffff)
    );
    assert_eq!(state.x87.tag, 0);
}

#[test]
fn a_backend_without_environment_support_reports_unsupported() {
    struct Unavailable;
    impl Guest for Unavailable {
        fn reg(&self, _: Reg) -> u32 {
            unreachable!()
        }
        fn set_reg(&mut self, _: Reg, _: u32) {
            unreachable!()
        }
        fn read(&self, _: u32, _: &mut [u8]) -> Result<(), MemFault> {
            unreachable!()
        }
        fn write(&mut self, _: u32, _: &[u8]) -> Result<(), MemFault> {
            unreachable!()
        }
        fn map(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
            unreachable!()
        }
        fn protect(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
            unreachable!()
        }
        fn unmap(&mut self, _: u32, _: u32) -> Result<(), MemFault> {
            unreachable!()
        }
    }
    assert_eq!(
        Unavailable.fpu_environment(),
        Err(FpuEnvironmentError::Unsupported)
    );
    assert_eq!(
        Unavailable.set_fpu_environment(FpuEnvironment {
            x87_control: 0x007f,
            mxcsr: 0x1f80
        }),
        Err(FpuEnvironmentError::Unsupported)
    );
}

#[test]
fn same_control_and_sse_only_changes_preserve_raw_x87_status() {
    let mut cpu = JitCpu::new(0x7e100000, false).unwrap();
    let state = &mut cpu.executor_mut().state;
    state.x87.control = 0x027f;
    state.x87.status = 0xbdc5;
    state.x87.ip = 0x12345678;
    state.x87.dp = 0x87654321;
    let before = state.x87.clone();
    let environment = cpu.fpu_environment().unwrap();
    cpu.set_fpu_environment(environment).unwrap();
    assert_eq!(cpu.executor().state.x87, before);
    cpu.set_fpu_environment(FpuEnvironment {
        mxcsr: 0x3fc3,
        ..environment
    })
    .unwrap();
    assert_eq!(cpu.executor().state.x87, before);
}
