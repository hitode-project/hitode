//! Field offsets and data addresses from the pinned win32u.so DWARF.
#![allow(non_upper_case_globals, dead_code)]

pub mod dib {
    pub const SIZE: usize = 0x64;
    pub const bit_count: usize = 0x00;
    pub const width: usize = 0x04;
    pub const height: usize = 0x08;
    pub const compression: usize = 0x0c;
    pub const rect: usize = 0x10;
    pub const stride: usize = 0x20;
    pub const bits: usize = 0x24;
    pub const red_mask: usize = 0x34;
    pub const green_mask: usize = 0x38;
    pub const blue_mask: usize = 0x3c;
    pub const red_shift: usize = 0x40;
    pub const green_shift: usize = 0x44;
    pub const blue_shift: usize = 0x48;
    pub const red_len: usize = 0x4c;
    pub const green_len: usize = 0x50;
    pub const blue_len: usize = 0x54;
    pub const color_table: usize = 0x58;
    pub const color_table_size: usize = 0x5c;
    pub const funcs: usize = 0x60;
}
pub mod bits {
    pub const SIZE: usize = 0x10;
    pub const ptr: usize = 0x00;
    pub const is_copy: usize = 0x04;
    pub const free: usize = 0x08;
    pub const param: usize = 0x0c;
}
pub mod rect {
    pub const SIZE: usize = 0x10;
    pub const left: usize = 0x00;
    pub const top: usize = 0x04;
    pub const right: usize = 0x08;
    pub const bottom: usize = 0x0c;
}
pub mod point {
    pub const SIZE: usize = 0x08;
    pub const x: usize = 0x00;
    pub const y: usize = 0x04;
}
pub mod line {
    pub const SIZE: usize = 0x20;
    pub const err_start: usize = 0x00;
    pub const err_add_1: usize = 0x04;
    pub const err_add_2: usize = 0x08;
    pub const bias: usize = 0x0c;
    pub const length: usize = 0x10;
    pub const x_inc: usize = 0x14;
    pub const y_inc: usize = 0x18;
    pub const x_major: usize = 0x1c;
}
pub mod stretch {
    pub const SIZE: usize = 0x18;
    pub const err_start: usize = 0x00;
    pub const err_add_1: usize = 0x04;
    pub const err_add_2: usize = 0x08;
    pub const length: usize = 0x0c;
    pub const dst_inc: usize = 0x10;
    pub const src_inc: usize = 0x14;
}
pub mod intensity {
    pub const SIZE: usize = 0x06;
    pub const r_min: usize = 0x00;
    pub const r_max: usize = 0x01;
    pub const g_min: usize = 0x02;
    pub const g_max: usize = 0x03;
    pub const b_min: usize = 0x04;
    pub const b_max: usize = 0x05;
}
pub mod gamma {
    pub const SIZE: usize = 0x0204;
    pub const gamma: usize = 0x00;
    pub const encode: usize = 0x04;
    pub const decode: usize = 0x0104;
}
pub mod coords {
    pub const SIZE: usize = 0x34;
    pub const log_x: usize = 0x00;
    pub const log_y: usize = 0x04;
    pub const log_width: usize = 0x08;
    pub const log_height: usize = 0x0c;
    pub const x: usize = 0x10;
    pub const y: usize = 0x14;
    pub const width: usize = 0x18;
    pub const height: usize = 0x1c;
    pub const visrect: usize = 0x20;
    pub const layout: usize = 0x30;
}
pub mod blend {
    pub const SIZE: usize = 0x04;
    pub const BlendOp: usize = 0x00;
    pub const BlendFlags: usize = 0x01;
    pub const SourceConstantAlpha: usize = 0x02;
    pub const AlphaFormat: usize = 0x03;
}
pub mod vertex {
    pub const SIZE: usize = 0x10;
    pub const x: usize = 0x00;
    pub const y: usize = 0x04;
    pub const Red: usize = 0x08;
    pub const Green: usize = 0x0a;
    pub const Blue: usize = 0x0c;
    pub const Alpha: usize = 0x0e;
}
pub mod rgb {
    pub const SIZE: usize = 0x04;
    pub const rgbBlue: usize = 0x00;
    pub const rgbGreen: usize = 0x01;
    pub const rgbRed: usize = 0x02;
    pub const rgbReserved: usize = 0x03;
}
pub mod masks {
    pub const SIZE: usize = 0x08;
    pub const and_: usize = 0x00;
    pub const xor_: usize = 0x04;
}
pub const FUNCS_8888: u32 = 0x1d01a0;
pub const FUNCS_32: u32 = 0x1d0140;
pub const FUNCS_24: u32 = 0x1d00e0;
pub const FUNCS_555: u32 = 0x1d0080;
pub const FUNCS_16: u32 = 0x1d0020;
pub const FUNCS_8: u32 = 0x1cffc0;
pub const FUNCS_4: u32 = 0x1cff60;
pub const FUNCS_1: u32 = 0x1cff00;
pub const TABLE_1: u32 = 0x15c200;
pub const TABLE_4: u32 = 0x15c1c0;
pub const TABLE_8: u32 = 0x15bdc0;
