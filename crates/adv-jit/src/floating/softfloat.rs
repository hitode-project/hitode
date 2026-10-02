//! Safe per-operation interface to the TLS SoftFloat C ABI shim.
use crate::X87Register;
#[repr(C)]
#[derive(Default)]
struct Extended {
    significand: u64,
    sign_exponent: u16,
}
/// Binary128 intermediate. Low limb first, matching the little-endian SoftFloat ABI.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Quad(pub [u64; 2]);
impl From<X87Register> for Extended {
    fn from(v: X87Register) -> Self {
        Self {
            significand: v.significand(),
            sign_exponent: v.sign_exponent(),
        }
    }
}
impl From<Extended> for X87Register {
    fn from(v: Extended) -> Self {
        Self::from_parts(v.significand, v.sign_exponent)
    }
}
unsafe extern "C" {
    fn adv_sf_config(rounding: u8, precision: u8);
    fn adv_sf_flags() -> u8;
    fn adv_sf_round_up() -> u8;
    fn adv_sf_ext_binary(a: *const Extended, b: *const Extended, out: *mut Extended, op: u8);
    fn adv_sf_ext_unary(a: *const Extended, out: *mut Extended, op: u8);
    fn adv_sf_ext_from(bits: u64, kind: u8, out: *mut Extended);
    fn adv_sf_ext_to(a: *const Extended, kind: u8, rounding: u8) -> u64;
    fn adv_sf_ext_compare(a: *const Extended, b: *const Extended, signal: u8) -> u8;
    fn adv_sf_sse_binary(a: u64, b: u64, op: u8, width: u8) -> u64;
    fn adv_sf_sse_convert(bits: u64, from: u8, to: u8, rounding: u8) -> u64;
    fn adv_sf_quad_binary(a: *const Quad, b: *const Quad, out: *mut Quad, op: u8);
    fn adv_sf_quad_from_ext(a: *const Extended, out: *mut Quad);
    fn adv_sf_quad_to_ext(a: *const Quad, out: *mut Extended);
    fn adv_sf_quad_from_i32(a: i32, out: *mut Quad);
    fn adv_sf_quad_to_i32(a: *const Quad, rounding: u8) -> i32;
    fn adv_sf_quad_to_i64(a: *const Quad, rounding: u8) -> i64;
    fn adv_sf_quad_round(a: *const Quad, out: *mut Quad, rounding: u8);
    fn adv_sf_quad_lt(a: *const Quad, b: *const Quad) -> u8;
    fn adv_sf_ext_scale(a: *const Extended, count: i32, out: *mut Extended);
}
pub(super) fn rounding(rc: u16) -> u8 {
    [0, 2, 3, 1][(rc & 3) as usize]
}
pub(super) fn configure(control: u16) {
    let precision = match (control >> 8) & 3 {
        0 => 32,
        2 => 64,
        _ => 80,
    };
    // SAFETY: the shim modifies only this thread's SoftFloat operation settings.
    unsafe { adv_sf_config(rounding(control >> 10), precision) }
}
pub(super) fn configure_sse(mxcsr: u32) {
    unsafe { adv_sf_config(rounding((mxcsr >> 13) as u16), 80) }
}
pub(super) fn flags() -> u16 {
    let sf = unsafe { adv_sf_flags() };
    ((sf & 16) >> 4) as u16
        | ((sf & 8) >> 1) as u16
        | ((sf & 4) << 1) as u16
        | ((sf & 2) << 3) as u16
        | ((sf & 1) << 5) as u16
}
pub(super) fn binary(a: X87Register, b: X87Register, op: u8) -> X87Register {
    let mut out = Extended::default();
    unsafe { adv_sf_ext_binary(&a.into(), &b.into(), &mut out, op) };
    out.into()
}
pub(super) fn rounded_up() -> bool {
    unsafe { adv_sf_round_up() != 0 }
}
pub(super) fn unary(a: X87Register, op: u8) -> X87Register {
    let mut out = Extended::default();
    unsafe { adv_sf_ext_unary(&a.into(), &mut out, op) };
    out.into()
}
pub(super) fn from(bits: u64, kind: u8) -> X87Register {
    let mut out = Extended::default();
    unsafe { adv_sf_ext_from(bits, kind, &mut out) };
    out.into()
}
pub(super) fn to(a: X87Register, kind: u8, rc: u8) -> u64 {
    unsafe { adv_sf_ext_to(&a.into(), kind, rc) }
}
pub(super) fn compare(a: X87Register, b: X87Register, signal: bool) -> u8 {
    unsafe { adv_sf_ext_compare(&a.into(), &b.into(), u8::from(signal)) }
}
pub(super) fn sse_binary(a: u64, b: u64, op: u8, width: u8) -> u64 {
    unsafe { adv_sf_sse_binary(a, b, op, width) }
}
pub(super) fn sse_convert(bits: u64, from: u8, to: u8, rc: u8) -> u64 {
    unsafe { adv_sf_sse_convert(bits, from, to, rc) }
}
pub(super) fn scale(a: X87Register, count: i32) -> X87Register {
    let mut out = Extended::default();
    unsafe { adv_sf_ext_scale(&a.into(), count, &mut out) };
    out.into()
}
impl Quad {
    pub const ZERO: Self = Self([0, 0]);
    pub const ONE: Self = Self([0, 0x3fff_0000_0000_0000]);
    pub const LN2: Self = Self([0xf357_93c7_6730_07e6, 0x3ffe_62e4_2fef_a39e]);
    pub const PI: Self = Self([0x8469_898c_c517_01b8, 0x4000_921f_b544_42d1]);
    pub fn from_ext(a: X87Register) -> Self {
        let mut out = Self::default();
        unsafe { adv_sf_quad_from_ext(&a.into(), &mut out) };
        out
    }
    pub fn to_ext(self) -> X87Register {
        let mut out = Extended::default();
        unsafe { adv_sf_quad_to_ext(&self, &mut out) };
        out.into()
    }
    pub fn integer(a: i32) -> Self {
        let mut out = Self::default();
        unsafe { adv_sf_quad_from_i32(a, &mut out) };
        out
    }
    pub fn to_i32(self, rounding: u8) -> i32 {
        unsafe { adv_sf_quad_to_i32(&self, rounding) }
    }
    pub fn to_i64(self, rounding: u8) -> i64 {
        unsafe { adv_sf_quad_to_i64(&self, rounding) }
    }
    pub fn round(self, rounding: u8) -> Self {
        let mut out = Self::default();
        unsafe { adv_sf_quad_round(&self, &mut out, rounding) };
        out
    }
    pub fn neg(mut self) -> Self {
        self.0[1] ^= 1 << 63;
        self
    }
    pub fn abs(mut self) -> Self {
        self.0[1] &= !(1 << 63);
        self
    }
    pub fn lt(self, other: Self) -> bool {
        unsafe { adv_sf_quad_lt(&self, &other) != 0 }
    }
    fn binary(self, other: Self, op: u8) -> Self {
        let mut out = Self::default();
        unsafe { adv_sf_quad_binary(&self, &other, &mut out, op) };
        out
    }
    pub fn add(self, other: Self) -> Self {
        self.binary(other, 0)
    }
    pub fn sub(self, other: Self) -> Self {
        self.binary(other, 1)
    }
    pub fn mul(self, other: Self) -> Self {
        self.binary(other, 2)
    }
    pub fn div(self, other: Self) -> Self {
        self.binary(other, 3)
    }
}
