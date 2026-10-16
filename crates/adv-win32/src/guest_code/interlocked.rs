//! Authored x86 stdcall atomics. LOCK preserves shared-memory ordering; XADD
//! returns the old word, while Increment/Decrement return the wrapped new word.
use adv_abi::GuestCode;

pub(super) static INCREMENT: GuestCode = GuestCode {
    // mov edx,[esp+4]; mov eax,1; lock xadd [edx],eax; inc eax; ret 4
    code: &[
        0x8b, 0x54, 0x24, 4, 0xb8, 1, 0, 0, 0, 0xf0, 0x0f, 0xc1, 2, 0x40, 0xc2, 4, 0,
    ],
    host: &[],
    links: &[],
};
pub(super) static DECREMENT: GuestCode = GuestCode {
    // mov edx,[esp+4]; mov eax,-1; lock xadd [edx],eax; dec eax; ret 4
    code: &[
        0x8b, 0x54, 0x24, 4, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xf0, 0x0f, 0xc1, 2, 0x48, 0xc2, 4, 0,
    ],
    host: &[],
    links: &[],
};
pub(super) static EXCHANGE: GuestCode = GuestCode {
    // mov edx,[esp+4]; mov eax,[esp+8]; lock xchg [edx],eax; ret 8
    code: &[
        0x8b, 0x54, 0x24, 4, 0x8b, 0x44, 0x24, 8, 0xf0, 0x87, 2, 0xc2, 8, 0,
    ],
    host: &[],
    links: &[],
};
pub(super) static EXCHANGE_ADD: GuestCode = GuestCode {
    // mov edx,[esp+4]; mov eax,[esp+8]; lock xadd [edx],eax; ret 8
    code: &[
        0x8b, 0x54, 0x24, 4, 0x8b, 0x44, 0x24, 8, 0xf0, 0x0f, 0xc1, 2, 0xc2, 8, 0,
    ],
    host: &[],
    links: &[],
};
pub(super) static COMPARE_EXCHANGE: GuestCode = GuestCode {
    // mov edx,[esp+4]; mov ecx,[esp+8]; mov eax,[esp+12];
    // lock cmpxchg [edx],ecx; ret 12
    code: &[
        0x8b, 0x54, 0x24, 4, 0x8b, 0x4c, 0x24, 8, 0x8b, 0x44, 0x24, 12, 0xf0, 0x0f, 0xb1, 0x0a,
        0xc2, 12, 0,
    ],
    host: &[],
    links: &[],
};
