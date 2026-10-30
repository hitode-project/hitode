//! The pinned mixer's SSE arithmetic and x87 float returns, under guest controls.
use super::Error;
use adv_cpu::backend::{FaultKind, Guest};
use adv_cpu::floating::softfloat as sf;

#[derive(Clone, Copy)]
pub(super) struct Float {
    mxcsr: u32,
    control: u16,
    status: u16,
}
impl Float {
    pub fn new(cpu: &mut dyn Guest) -> Result<Self, String> {
        let state = cpu.capture_architecture().map_err(|e| e.to_string())?;
        Ok(Self {
            mxcsr: state.mxcsr,
            control: state.x87.control,
            status: state.x87.status,
        })
    }
    pub fn publish(self, cpu: &mut dyn Guest, entry: u32) {
        if let Ok(mut state) = cpu.capture_architecture() {
            state.mxcsr = self.mxcsr;
            state.x87.status = self.status;
            state.x87.ip = entry;
            let _ = cpu.restore_architecture(&state);
        }
    }
    fn finish(&mut self, flags: u16) -> Result<(), Error> {
        self.mxcsr |= u32::from(flags);
        if u32::from(flags) & !(self.mxcsr >> 7) & 0x3f != 0 {
            return Err(Error::Fault(FaultKind::Trap(19)));
        }
        Ok(())
    }
    fn denormal(a: u64, width: u8) -> bool {
        let (exp, fraction) = if width == 4 {
            (0x7f80_0000, 0x7f_ffff)
        } else {
            (0x7ff0_0000_0000_0000, 0xf_ffff_ffff_ffff)
        };
        a & exp == 0 && a & fraction != 0
    }
    fn input(&mut self, mut a: u64, width: u8) -> Result<u64, Error> {
        if Self::denormal(a, width) {
            if self.mxcsr & 0x40 != 0 {
                a &= if width == 4 { 1 << 31 } else { 1 << 63 };
            } else {
                self.finish(2)?;
            }
        }
        Ok(a)
    }
    fn output(&mut self, mut a: u64, width: u8) -> Result<u64, Error> {
        self.finish(sf::flags())?;
        if self.mxcsr & 0x8000 != 0 && Self::denormal(a, width) {
            self.finish(0x30)?;
            a &= if width == 4 { 1 << 31 } else { 1 << 63 };
        }
        Ok(a)
    }
    fn binary(&mut self, a: u64, b: u64, op: u8, width: u8) -> Result<u64, Error> {
        // Masked, nearest arithmetic on finite normal values is native.
        // Sticky exception flags are caller-clobbered by the guest ABI.
        if self.mxcsr & !0x3f == 0x1f80 {
            if width == 4 {
                let (x, y) = (f32::from_bits(a as u32), f32::from_bits(b as u32));
                if x.is_finite() && y.is_finite() && !x.is_subnormal() && !y.is_subnormal() {
                    let result = match op {
                        0 => x + y,
                        1 => x - y,
                        2 => x * y,
                        _ => x / y,
                    };
                    if result.is_finite() && !result.is_subnormal() {
                        return Ok(result.to_bits().into());
                    }
                }
            } else {
                let (x, y) = (f64::from_bits(a), f64::from_bits(b));
                if x.is_finite() && y.is_finite() && !x.is_subnormal() && !y.is_subnormal() {
                    let result = match op {
                        0 => x + y,
                        1 => x - y,
                        2 => x * y,
                        _ => x / y,
                    };
                    if result.is_finite() && !result.is_subnormal() {
                        return Ok(result.to_bits());
                    }
                }
            }
        }
        let a = self.input(a, width)?;
        let b = self.input(b, width)?;
        sf::configure_sse(self.mxcsr);
        let result = sf::sse_binary(a, b, op, width);
        self.output(result, width)
    }
    pub fn add(&mut self, a: f32, b: f32) -> Result<f32, Error> {
        Ok(f32::from_bits(
            self.binary(a.to_bits().into(), b.to_bits().into(), 0, 4)? as u32,
        ))
    }
    pub fn mul(&mut self, a: f32, b: f32) -> Result<f32, Error> {
        Ok(f32::from_bits(
            self.binary(a.to_bits().into(), b.to_bits().into(), 2, 4)? as u32,
        ))
    }
    pub fn div(&mut self, a: f32, b: f32) -> Result<f32, Error> {
        Ok(f32::from_bits(
            self.binary(a.to_bits().into(), b.to_bits().into(), 3, 4)? as u32,
        ))
    }
    pub fn double(&mut self, a: f64, b: f64, op: u8) -> Result<f64, Error> {
        Ok(f64::from_bits(self.binary(
            a.to_bits(),
            b.to_bits(),
            op,
            8,
        )?))
    }
    pub fn cast(&mut self, a: u64, from: u8, to: u8) -> Result<u64, Error> {
        let a = if from != 2 {
            self.input(a, if from == 0 { 4 } else { 8 })?
        } else {
            a
        };
        sf::configure_sse(self.mxcsr);
        let result = sf::sse_convert(a, from, to, sf::rounding((self.mxcsr >> 13) as u16));
        self.output(result, if to == 0 { 4 } else { 8 })
    }
    pub fn int(&mut self, a: i32) -> Result<f32, Error> {
        Ok(f32::from_bits(self.cast(a as u32 as u64, 2, 0)? as u32))
    }
    pub fn narrow(&mut self, a: f64) -> Result<f32, Error> {
        Ok(f32::from_bits(self.cast(a.to_bits(), 1, 0)? as u32))
    }
    pub fn widen(&mut self, a: f32) -> Result<f64, Error> {
        Ok(f64::from_bits(self.cast(a.to_bits().into(), 0, 1)?))
    }
    fn wait(&self) -> Result<(), Error> {
        if self.status & !self.control & 0x3f != 0 {
            Err(Error::Fault(FaultKind::Trap(16)))
        } else {
            Ok(())
        }
    }
    fn x87(&mut self, bits: u64, kind: u8) -> Result<f32, Error> {
        self.wait()?;
        sf::configure(self.control);
        let value = sf::from(bits, kind);
        self.status |= sf::flags();
        if kind == 0 && Self::denormal(bits, 4) {
            self.status |= 2;
        }
        self.wait()?;
        sf::configure(self.control);
        let result = sf::to(value, 0, sf::rounding(self.control >> 10));
        self.status |= sf::flags();
        self.wait()?;
        Ok(f32::from_bits(result as u32))
    }
    pub fn returned(&mut self, a: f32) -> Result<f32, Error> {
        self.x87(a.to_bits().into(), 0)
    }
    pub fn long(&mut self, a: i64) -> Result<f32, Error> {
        self.x87(a as u64, 3)
    }
}
