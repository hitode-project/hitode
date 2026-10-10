//! Fixed-width packed integer kernels shared by decoded and generic execution.
use iced_x86::Mnemonic as M;

pub type Kernel = fn([u8; 16], [u8; 16]) -> [u8; 16];

macro_rules! lanes {
    ($name:ident, $ty:ty, |$a:ident, $b:ident| $value:expr) => {
        fn $name(left: [u8; 16], right: [u8; 16]) -> [u8; 16] {
            let mut out = [0; 16];
            for ((a, b), output) in left
                .chunks_exact(size_of::<$ty>())
                .zip(right.chunks_exact(size_of::<$ty>()))
                .zip(out.chunks_exact_mut(size_of::<$ty>()))
            {
                let $a = <$ty>::from_le_bytes(a.try_into().unwrap());
                let $b = <$ty>::from_le_bytes(b.try_into().unwrap());
                let value: $ty = $value;
                output.copy_from_slice(&value.to_le_bytes());
            }
            out
        }
    };
}

lanes!(add8, u8, |a, b| a.wrapping_add(b));
lanes!(add16, u16, |a, b| a.wrapping_add(b));
lanes!(add32, u32, |a, b| a.wrapping_add(b));
lanes!(add64, u64, |a, b| a.wrapping_add(b));
lanes!(sub8, u8, |a, b| a.wrapping_sub(b));
lanes!(sub16, u16, |a, b| a.wrapping_sub(b));
lanes!(sub32, u32, |a, b| a.wrapping_sub(b));
lanes!(sub64, u64, |a, b| a.wrapping_sub(b));
lanes!(adds8, i8, |a, b| a.saturating_add(b));
lanes!(adds16, i16, |a, b| a.saturating_add(b));
lanes!(addu8, u8, |a, b| a.saturating_add(b));
lanes!(addu16, u16, |a, b| a.saturating_add(b));
lanes!(subs8, i8, |a, b| a.saturating_sub(b));
lanes!(subs16, i16, |a, b| a.saturating_sub(b));
lanes!(subu8, u8, |a, b| a.saturating_sub(b));
lanes!(subu16, u16, |a, b| a.saturating_sub(b));
lanes!(
    avg8,
    u8,
    |a, b| (u16::from(a) + u16::from(b) + 1).wrapping_shr(1) as u8
);
lanes!(
    avg16,
    u16,
    |a, b| (u32::from(a) + u32::from(b) + 1).wrapping_shr(1) as u16
);
lanes!(eq8, u8, |a, b| if a == b { !0 } else { 0 });
lanes!(eq16, u16, |a, b| if a == b { !0 } else { 0 });
lanes!(eq32, u32, |a, b| if a == b { !0 } else { 0 });
lanes!(gt8, i8, |a, b| if a > b { -1 } else { 0 });
lanes!(gt16, i16, |a, b| if a > b { -1 } else { 0 });
lanes!(gt32, i32, |a, b| if a > b { -1 } else { 0 });
lanes!(mullo16, u16, |a, b| a.wrapping_mul(b));
lanes!(mulhi16, i16, |a, b| ((i32::from(a) * i32::from(b)) >> 16)
    as i16);
lanes!(mulhiu16, u16, |a, b| ((u32::from(a) * u32::from(b)) >> 16)
    as u16);
lanes!(minu8, u8, |a, b| a.min(b));
lanes!(maxu8, u8, |a, b| a.max(b));
lanes!(mins16, i16, |a, b| a.min(b));
lanes!(maxs16, i16, |a, b| a.max(b));
lanes!(xor, u64, |a, b| a ^ b);
lanes!(and, u64, |a, b| a & b);
lanes!(andnot, u64, |a, b| !a & b);
lanes!(or, u64, |a, b| a | b);

macro_rules! shift {
    ($name:ident, $ty:ty, |$a:ident, $n:ident| $value:expr) => {
        fn $name(left: [u8; 16], right: [u8; 16]) -> [u8; 16] {
            let $n = u64::from_le_bytes(right[..8].try_into().unwrap());
            let mut out = [0; 16];
            for (a, output) in left
                .chunks_exact(size_of::<$ty>())
                .zip(out.chunks_exact_mut(size_of::<$ty>()))
            {
                let $a = <$ty>::from_le_bytes(a.try_into().unwrap());
                let value: $ty = $value;
                output.copy_from_slice(&value.to_le_bytes());
            }
            out
        }
    };
}
shift!(shl16, u16, |a, n| if n >= 16 { 0 } else { a << n });
shift!(shl32, u32, |a, n| if n >= 32 { 0 } else { a << n });
shift!(shl64, u64, |a, n| if n >= 64 { 0 } else { a << n });
shift!(shr16, u16, |a, n| if n >= 16 { 0 } else { a >> n });
shift!(shr32, u32, |a, n| if n >= 32 { 0 } else { a >> n });
shift!(shr64, u64, |a, n| if n >= 64 { 0 } else { a >> n });
shift!(sar16, i16, |a, n| a >> n.min(15));
shift!(sar32, i32, |a, n| a >> n.min(31));

pub fn kernel(m: M) -> Option<Kernel> {
    Some(match m {
        M::Paddb => add8,
        M::Paddw => add16,
        M::Paddd => add32,
        M::Paddq => add64,
        M::Psubb => sub8,
        M::Psubw => sub16,
        M::Psubd => sub32,
        M::Psubq => sub64,
        M::Paddsb => adds8,
        M::Paddsw => adds16,
        M::Paddusb => addu8,
        M::Paddusw => addu16,
        M::Psubsb => subs8,
        M::Psubsw => subs16,
        M::Psubusb => subu8,
        M::Psubusw => subu16,
        M::Pavgb => avg8,
        M::Pavgw => avg16,
        M::Pcmpeqb => eq8,
        M::Pcmpeqw => eq16,
        M::Pcmpeqd => eq32,
        M::Pcmpgtb => gt8,
        M::Pcmpgtw => gt16,
        M::Pcmpgtd => gt32,
        M::Pmullw => mullo16,
        M::Pmulhw => mulhi16,
        M::Pmulhuw => mulhiu16,
        M::Pminub => minu8,
        M::Pmaxub => maxu8,
        M::Pminsw => mins16,
        M::Pmaxsw => maxs16,
        M::Xorps | M::Xorpd | M::Pxor => xor,
        M::Andps | M::Andpd | M::Pand => and,
        M::Andnps | M::Andnpd | M::Pandn => andnot,
        M::Orps | M::Orpd | M::Por => or,
        M::Psllw => shl16,
        M::Pslld => shl32,
        M::Psllq => shl64,
        M::Psrlw => shr16,
        M::Psrld => shr32,
        M::Psrlq => shr64,
        M::Psraw => sar16,
        M::Psrad => sar32,
        _ => return None,
    })
}
