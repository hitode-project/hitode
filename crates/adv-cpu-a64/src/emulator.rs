//! Unicorn arm64 harness for running translated code on any host.
//!
//! M0 adds the guest window, guard zones and soft pages as memory hooks; the
//! fault paths then go through the same side-table logic as on hardware.

use unicorn_engine::unicorn_const::{Arch, Mode, Prot, uc_error};
use unicorn_engine::{RegisterARM64, Unicorn};

use crate::encode::Reg;

const CODE: u64 = 0x1000_0000;
const CODE_SIZE: u64 = 0x1_0000;
/// Return address planted in LR: the run stops when code returns here.
const DONE: u64 = CODE + CODE_SIZE - 4;
/// Instruction budget, so a broken branch cannot hang a test.
const BUDGET: usize = 1 << 20;

pub(crate) struct Emulator {
    uc: Unicorn<'static, ()>,
}

fn register(reg: Reg) -> RegisterARM64 {
    // Unicorn keeps X29 and X30 apart from X0..X28 as FP and LR.
    match reg.number() {
        n @ 0..=28 => {
            let id = RegisterARM64::X0 as i32 + n as i32;
            // SAFETY: X0..X28 are consecutive discriminants of a C enum.
            unsafe { std::mem::transmute::<i32, RegisterARM64>(id) }
        }
        29 => RegisterARM64::FP,
        30 => RegisterARM64::LR,
        _ => panic!("register 31 is ZR or SP, not a general register"),
    }
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let mut uc = Unicorn::new(Arch::ARM64, Mode::ARM).expect("unicorn arm64");
        uc.mem_map(CODE, CODE_SIZE, Prot::READ | Prot::EXEC)
            .expect("map code");
        Self { uc }
    }

    pub(crate) fn x(&self, reg: Reg) -> u64 {
        self.uc.reg_read(register(reg)).expect("read register")
    }

    pub(crate) fn set_x(&mut self, reg: Reg, value: u64) {
        self.uc
            .reg_write(register(reg), value)
            .expect("write register");
    }

    /// Runs `code` from its first word until it returns through LR.
    pub(crate) fn call(&mut self, code: &[u32]) -> Result<(), uc_error> {
        let bytes: Vec<u8> = code.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert!((bytes.len() as u64) < DONE - CODE, "code does not fit");
        self.uc.mem_write(CODE, &bytes)?;
        self.uc.reg_write(RegisterARM64::LR, DONE)?;
        self.uc.emu_start(CODE, DONE, 0, BUDGET)?;
        let pc = self.uc.reg_read(RegisterARM64::PC)?;
        assert_eq!(pc, DONE, "stopped at {pc:#x} instead of returning");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Shift::*;
    use crate::encode::Size::*;
    use crate::encode::*;

    const fn r(number: u8) -> Reg {
        Reg::new(number)
    }

    #[test]
    fn every_general_register_round_trips() {
        let mut emu = Emulator::new();
        for n in 0..31 {
            emu.set_x(r(n), 0x1111_0000_0000_0000 * (n as u64 % 15 + 1) + n as u64);
        }
        for n in 0..31 {
            assert_eq!(
                emu.x(r(n)),
                0x1111_0000_0000_0000 * (n as u64 % 15 + 1) + n as u64
            );
        }
    }

    #[test]
    fn biased_stack_pointer_and_constants_execute_as_encoded() {
        let mut emu = Emulator::new();
        emu.set_x(r(23), 0xFFFF_FFFF_0012_FF80); // only the low 32 bits are ESP
        emu.set_x(r(19), 0xDEAD_BEEF_0000_0001);
        emu.set_x(r(20), 0xFFFF_FFFF);
        emu.set_x(r(2), 0x8000_0000);
        let code = [
            movz(X, r(27), 1, 32),                 // window base 4 GiB
            add(W, r(23), r(23), Reg::ZR, Lsl, 0), // zero-extend ESP
            add(X, r(23), r(27), r(23), Lsl, 0),   // bias by the window base
            add(W, r(19), r(19), r(20), Lsl, 0),   // 32-bit add wraps, clears the top
            movz(X, r(0), 0x5678, 0),
            movk(X, r(0), 0x1234, 16),
            sub(W, r(1), Reg::ZR, r(2), Asr, 31), // 0 - (-1)
            NOP,
            ret(Reg::LR),
        ];
        emu.call(&code).unwrap();
        assert_eq!(emu.x(r(23)), 0x1_0012_FF80);
        assert_eq!(emu.x(r(19)), 0);
        assert_eq!(emu.x(r(0)), 0x1234_5678);
        assert_eq!(emu.x(r(1)), 1);
    }

    #[test]
    fn a_trap_stops_the_run_with_an_error() {
        let mut emu = Emulator::new();
        assert!(emu.call(&[brk(0xf00d), ret(Reg::LR)]).is_err());
    }
}
