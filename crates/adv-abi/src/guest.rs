//! Guest values are addresses and copies, never host references into guest storage.
use core::marker::PhantomData;

use adv_cpu::backend::{Guest, GuestExt, MemFault, Prot, Reg};
use adv_machine::Outcome;

use crate::AbiKind;

#[repr(transparent)]
pub struct GuestPtr<T> {
    address: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Copy for GuestPtr<T> {}
impl<T> Clone for GuestPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> core::fmt::Debug for GuestPtr<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "GuestPtr({:#010x})", self.address)
    }
}
impl<T> PartialEq for GuestPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}
impl<T> Eq for GuestPtr<T> {}
impl<T> Default for GuestPtr<T> {
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T> GuestPtr<T> {
    pub const fn new(address: u32) -> Self {
        Self {
            address,
            marker: PhantomData,
        }
    }
    pub const fn addr(self) -> u32 {
        self.address
    }
    pub const fn is_null(self) -> bool {
        self.address == 0
    }
    pub const fn cast<U>(self) -> GuestPtr<U> {
        GuestPtr::new(self.address)
    }
    pub fn offset(self, count: u32) -> Result<Self, MemFault> {
        let length = count.checked_mul(core::mem::size_of::<T>() as u32);
        length
            .and_then(|len| self.address.checked_add(len))
            .map(Self::new)
            .ok_or(MemFault {
                addr: self.address,
                len: u32::MAX,
            })
    }
}

/// Enforce guest page permissions even when a backend's host read/write bypasses them.
pub fn check_range(
    cpu: &dyn Guest,
    address: u32,
    length: u32,
    protection: Prot,
) -> Result<(), MemFault> {
    if length == 0 {
        return Ok(());
    }
    if address == 0
        || address.checked_add(length - 1).is_none()
        || !cpu.accessible(address, length, protection)
    {
        return Err(MemFault {
            addr: address,
            len: length,
        });
    }
    Ok(())
}

impl<T: bytemuck::Pod> GuestPtr<T> {
    pub fn read(self, cpu: &dyn Guest) -> Result<T, MemFault> {
        let length = core::mem::size_of::<T>();
        check_range(cpu, self.address, length as u32, Prot::READ)?;
        let mut value: T = bytemuck::Zeroable::zeroed();
        cpu.read(self.address, bytemuck::bytes_of_mut(&mut value))?;
        Ok(value)
    }
    pub fn write(self, cpu: &mut dyn Guest, value: T) -> Result<(), MemFault> {
        check_range(
            cpu,
            self.address,
            core::mem::size_of::<T>() as u32,
            Prot::WRITE,
        )?;
        cpu.write(self.address, bytemuck::bytes_of(&value))
    }
}

/// A Windows handle value; host pointers and `usize` cannot enter the x86 ABI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct Handle(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnsiStr(pub GuestPtr<u8>);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WideStr(pub GuestPtr<u16>);

impl AnsiStr {
    pub fn read(self, cpu: &dyn Guest, limit: usize) -> Result<Vec<u8>, MemFault> {
        let mut bytes = Vec::new();
        for i in 0..limit {
            let p = self.0.offset(u32::try_from(i).map_err(|_| MemFault {
                addr: self.0.addr(),
                len: u32::MAX,
            })?)?;
            let byte = p.read(cpu)?;
            if byte == 0 {
                return Ok(bytes);
            }
            bytes.push(byte);
        }
        Err(MemFault {
            addr: self.0.addr(),
            len: u32::try_from(limit).unwrap_or(u32::MAX),
        })
    }
}
impl WideStr {
    pub fn read(self, cpu: &dyn Guest, limit: usize) -> Result<Vec<u16>, MemFault> {
        let mut units = Vec::new();
        for i in 0..limit {
            let p = self.0.offset(u32::try_from(i).map_err(|_| MemFault {
                addr: self.0.addr(),
                len: u32::MAX,
            })?)?;
            let unit = p.read(cpu)?;
            if unit == 0 {
                return Ok(units);
            }
            units.push(unit);
        }
        Err(MemFault {
            addr: self.0.addr(),
            len: u32::try_from(limit.saturating_mul(2)).unwrap_or(u32::MAX),
        })
    }
}

/// Parameter cursor; a 64-bit value consumes two stack words.
pub struct Arguments<'a> {
    cpu: &'a dyn Guest,
    word: u32,
}
impl<'a> Arguments<'a> {
    pub fn new(cpu: &'a dyn Guest) -> Self {
        Self { cpu, word: 0 }
    }
    pub fn next<T: GuestArg>(&mut self) -> Result<T, MemFault> {
        T::decode(self)
    }
    /// x86 aggregate parameters are copied by value and rounded up to stack words.
    pub fn pod<T: bytemuck::Pod>(&mut self) -> Result<T, MemFault> {
        let length = core::mem::size_of::<T>() as u32;
        let addr = self
            .cpu
            .reg(Reg::Esp)
            .checked_add(4)
            .and_then(|a| self.word.checked_mul(4).and_then(|off| a.checked_add(off)))
            .ok_or(MemFault {
                addr: self.cpu.reg(Reg::Esp),
                len: length,
            })?;
        check_range(self.cpu, addr, length, Prot::READ)?;
        let mut value: T = bytemuck::Zeroable::zeroed();
        self.cpu.read(addr, bytemuck::bytes_of_mut(&mut value))?;
        self.word += length.div_ceil(4);
        Ok(value)
    }
    fn word(&mut self) -> Result<u32, MemFault> {
        let addr = self
            .cpu
            .reg(Reg::Esp)
            .checked_add(4)
            .and_then(|a| self.word.checked_mul(4).and_then(|off| a.checked_add(off)))
            .ok_or(MemFault {
                addr: self.cpu.reg(Reg::Esp),
                len: 4,
            })?;
        check_range(self.cpu, addr, 4, Prot::READ)?;
        let result = self.cpu.read_u32(addr)?;
        self.word += 1;
        Ok(result)
    }
}

pub trait GuestArg: Sized {
    const KIND: AbiKind;
    const WORDS: u8;
    fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault>;
}

macro_rules! word_arg {
    ($ty:ty, $kind:ident, $convert:expr) => {
        impl GuestArg for $ty {
            const KIND: AbiKind = AbiKind::$kind;
            const WORDS: u8 = 1;
            fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault> {
                Ok(($convert)(args.word()?))
            }
        }
    };
}
word_arg!(u32, U32, |v| v);
word_arg!(i32, I32, |v| v as i32);
word_arg!(f32, F32, f32::from_bits);
word_arg!(Handle, Handle, Handle);
word_arg!(AnsiStr, AnsiString, |v| AnsiStr(GuestPtr::new(v)));
word_arg!(WideStr, WideString, |v| WideStr(GuestPtr::new(v)));
impl<T> GuestArg for GuestPtr<T> {
    const KIND: AbiKind = AbiKind::Pointer;
    const WORDS: u8 = 1;
    fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault> {
        Ok(Self::new(args.word()?))
    }
}
impl GuestArg for u64 {
    const KIND: AbiKind = AbiKind::U64;
    const WORDS: u8 = 2;
    fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault> {
        Ok(u64::from(args.word()?) | (u64::from(args.word()?) << 32))
    }
}
impl GuestArg for i64 {
    const KIND: AbiKind = AbiKind::I64;
    const WORDS: u8 = 2;
    fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault> {
        Ok(u64::decode(args)? as i64)
    }
}
impl GuestArg for f64 {
    const KIND: AbiKind = AbiKind::F64;
    const WORDS: u8 = 2;
    fn decode(args: &mut Arguments<'_>) -> Result<Self, MemFault> {
        Ok(f64::from_bits(u64::decode(args)?))
    }
}

pub trait GuestReturn {
    const KIND: AbiKind;
    fn into_outcome(self) -> Outcome;
}
impl GuestReturn for Outcome {
    const KIND: AbiKind = AbiKind::Outcome;
    fn into_outcome(self) -> Outcome {
        self
    }
}
impl GuestReturn for () {
    const KIND: AbiKind = AbiKind::Void;
    fn into_outcome(self) -> Outcome {
        Outcome::Return(0)
    }
}
impl GuestReturn for u32 {
    const KIND: AbiKind = AbiKind::U32;
    fn into_outcome(self) -> Outcome {
        Outcome::Return(self)
    }
}
impl GuestReturn for i32 {
    const KIND: AbiKind = AbiKind::I32;
    fn into_outcome(self) -> Outcome {
        Outcome::Return(self as u32)
    }
}
impl GuestReturn for u64 {
    const KIND: AbiKind = AbiKind::U64;
    fn into_outcome(self) -> Outcome {
        Outcome::Return64(self)
    }
}
impl GuestReturn for i64 {
    const KIND: AbiKind = AbiKind::I64;
    fn into_outcome(self) -> Outcome {
        Outcome::Return64(self as u64)
    }
}
impl GuestReturn for f32 {
    const KIND: AbiKind = AbiKind::F32;
    fn into_outcome(self) -> Outcome {
        Outcome::ReturnF32(self)
    }
}
impl GuestReturn for f64 {
    const KIND: AbiKind = AbiKind::F64;
    fn into_outcome(self) -> Outcome {
        Outcome::ReturnF64(self)
    }
}
impl GuestReturn for Handle {
    const KIND: AbiKind = AbiKind::Handle;
    fn into_outcome(self) -> Outcome {
        Outcome::Return(self.0)
    }
}
impl<T> GuestReturn for GuestPtr<T> {
    const KIND: AbiKind = AbiKind::Pointer;
    fn into_outcome(self) -> Outcome {
        Outcome::Return(self.addr())
    }
}
impl<T: GuestReturn> GuestReturn for Result<T, MemFault> {
    const KIND: AbiKind = T::KIND;
    fn into_outcome(self) -> Outcome {
        match self {
            Ok(v) => v.into_outcome(),
            Err(e) => memory_failure(e),
        }
    }
}

pub fn memory_failure(fault: MemFault) -> Outcome {
    Outcome::Fail(format!("ABI guest memory fault: {fault}"))
}
