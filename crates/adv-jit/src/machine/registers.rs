//! Fixed-size reads for the x86 registers synchronized at every fallback boundary.
//! Unicorn's general reg_read_long allocates a Box for each register read.
use unicorn_engine::{
    RegisterX86 as R, Unicorn,
    unicorn_const::{Arch, uc_error},
};

#[repr(C, align(16))]
struct RegisterBytes([u8; 16]);

fn read<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 16], uc_error> {
    if uc.get_arch() != Arch::X86 {
        return Err(uc_error::ARCH);
    }
    // This helper never accepts wider registers. In x86 mode, ST is 10 bytes
    // and XMM is 16 bytes; all other register layouts stay with the public API.
    if !matches!(
        register,
        R::ST0
            | R::ST1
            | R::ST2
            | R::ST3
            | R::ST4
            | R::ST5
            | R::ST6
            | R::ST7
            | R::XMM0
            | R::XMM1
            | R::XMM2
            | R::XMM3
            | R::XMM4
            | R::XMM5
            | R::XMM6
            | R::XMM7
    ) {
        return Err(uc_error::ARG);
    }
    let mut bytes = RegisterBytes([0; 16]);
    // SAFETY: uc owns a live x86 engine; the validated register writes at most
    // 16 bytes into aligned storage. The engine does not retain the pointer.
    let error = unsafe {
        unicorn_engine::uc_reg_read(
            uc.get_handle(),
            register as i32,
            bytes.0.as_mut_ptr().cast(),
        )
    };
    if error == uc_error::OK {
        Ok(bytes.0)
    } else {
        Err(error)
    }
}

pub(super) fn read_st<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 10], uc_error> {
    let bytes = read(uc, register)?;
    Ok(bytes[..10].try_into().unwrap())
}

pub(super) fn read_xmm<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 16], uc_error> {
    read(uc, register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicorn_engine::unicorn_const::Mode;

    #[test]
    fn fixed_reads_preserve_all_xmm_and_extended_float_bits() {
        let uc = Unicorn::new(Arch::X86, Mode::MODE_32).unwrap();
        for (index, register) in super::super::XMM.into_iter().enumerate() {
            let bytes: [u8; 16] = std::array::from_fn(|i| (index * 31 + i * 17) as u8);
            uc.reg_write_long(register, &bytes).unwrap();
            assert_eq!(read_xmm(&uc, register).unwrap(), bytes);
            assert_eq!(
                read_xmm(&uc, register).unwrap().as_slice(),
                &*uc.reg_read_long(register).unwrap()
            );
        }
        // Include finite, subnormal, infinity and NaN raw encodings; no conversion
        // through f64 may discard significand, exponent or payload bits.
        for (index, register) in super::super::ST.into_iter().enumerate() {
            let (mantissa, exponent): (u64, u16) = [
                (0, 0),
                (1, 0),
                (0x8000_0000_0000_0000, 0x3fff),
                (0x8000_0000_0000_0000, 0x7fff),
                (0, 0x8000),
                (0x8000_0000_0000_0000, 0xbfff),
                (0xc000_0000_0000_0042, 0xffff),
                (0xffff_ffff_ffff_ffff, 0x7ffe),
            ][index];
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
            bytes[8..].copy_from_slice(&exponent.to_le_bytes());
            uc.reg_write_long(register, &bytes).unwrap();
            assert_eq!(read_st(&uc, register).unwrap(), bytes);
            assert_eq!(
                read_st(&uc, register).unwrap().as_slice(),
                &*uc.reg_read_long(register).unwrap()
            );
        }
        assert_eq!(read(&uc, R::YMM0), Err(uc_error::ARG));
        assert_eq!(read(&uc, R::GDTR), Err(uc_error::ARG));
    }
}
