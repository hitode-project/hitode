//! Field offsets from the pinned dsound.dll's DWARF.
#![allow(non_upper_case_globals)]

pub mod device {
    pub const pwfx: usize = 0x7c;
    pub const nrofbuffers: usize = 0xa4;
    pub const buffers: usize = 0xa8;
    pub const tmp_buffer: usize = 0x108;
    pub const cp_buffer: usize = 0x10c;
    pub const tmp_buffer_len: usize = 0x110;
    pub const cp_buffer_len: usize = 0x114;
}
pub mod buffer {
    pub const device: usize = 0x28;
    pub const lock: usize = 0x2c;
    pub const pwfx: usize = 0x30;
    pub const buffer: usize = 0x34;
    pub const playflags: usize = 0x38;
    pub const state: usize = 0x3c;
    pub const writelead: usize = 0x40;
    pub const buflen: usize = 0x48;
    pub const volpan: usize = 0x50;
    pub const dsbd: usize = 0x70;
    pub const firstep: usize = 0x98;
    pub const firgain: usize = 0x9c;
    pub const freqAdjustNum: usize = 0xa0;
    pub const freqAdjustDen: usize = 0xa8;
    pub const freqAccNum: usize = 0xb0;
    pub const sec_mixpos: usize = 0xb8;
    pub const use_committed: usize = 0xbc;
    pub const committedbuff: usize = 0xc0;
    pub const committed_mixpos: usize = 0xc4;
    pub const notifies: usize = 0xc8;
    pub const nrofnotifies: usize = 0xcc;
    pub const mix_channels: usize = 0x11c;
    pub const get: usize = 0x120;
    pub const get_aux: usize = 0x124;
    pub const put: usize = 0x128;
    pub const put_aux: usize = 0x12c;
    pub const num_filters: usize = 0x130;
}
pub mod wave {
    pub const nChannels: usize = 2;
    pub const nBlockAlign: usize = 12;
}
pub mod volume {
    pub const dwTotalAmpFactor: usize = 0;
    pub const lVolume: usize = 24;
    pub const lPan: usize = 28;
}
pub mod desc {
    pub const dwFlags: usize = 4;
}
pub mod memory {
    pub const memory: usize = 8;
}
pub mod notify {
    pub const dwOffset: usize = 0;
}

pub const FIR_RVA: u32 = 0x25020;
pub const FIR_LEN: usize = 7907;
