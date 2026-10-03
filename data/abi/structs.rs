// Generated from pinned Windows.Win32.winmd, x86 only.
#![allow(non_snake_case)]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}
const _: () = assert!(core::mem::size_of::<FILETIME>() == 8);
const _: () = assert!(core::mem::align_of::<FILETIME>() == 4);
const _: () = assert!(core::mem::offset_of!(FILETIME, dwLowDateTime) == 0);
const _: () = assert!(core::mem::offset_of!(FILETIME, dwHighDateTime) == 4);
impl crate::GuestArg for FILETIME {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SYSTEMTIME {
    pub wYear: u16,
    pub wMonth: u16,
    pub wDayOfWeek: u16,
    pub wDay: u16,
    pub wHour: u16,
    pub wMinute: u16,
    pub wSecond: u16,
    pub wMilliseconds: u16,
}
const _: () = assert!(core::mem::size_of::<SYSTEMTIME>() == 16);
const _: () = assert!(core::mem::align_of::<SYSTEMTIME>() == 2);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wYear) == 0);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wMonth) == 2);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wDayOfWeek) == 4);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wDay) == 6);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wHour) == 8);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wMinute) == 10);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wSecond) == 12);
const _: () = assert!(core::mem::offset_of!(SYSTEMTIME, wMilliseconds) == 14);
impl crate::GuestArg for SYSTEMTIME {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}
const _: () = assert!(core::mem::size_of::<POINT>() == 8);
const _: () = assert!(core::mem::align_of::<POINT>() == 4);
const _: () = assert!(core::mem::offset_of!(POINT, x) == 0);
const _: () = assert!(core::mem::offset_of!(POINT, y) == 4);
impl crate::GuestArg for POINT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
const _: () = assert!(core::mem::size_of::<RECT>() == 16);
const _: () = assert!(core::mem::align_of::<RECT>() == 4);
const _: () = assert!(core::mem::offset_of!(RECT, left) == 0);
const _: () = assert!(core::mem::offset_of!(RECT, top) == 4);
const _: () = assert!(core::mem::offset_of!(RECT, right) == 8);
const _: () = assert!(core::mem::offset_of!(RECT, bottom) == 12);
impl crate::GuestArg for RECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SECURITY_ATTRIBUTES {
    pub nLength: u32,
    pub lpSecurityDescriptor: u32,
    pub bInheritHandle: i32,
}
const _: () = assert!(core::mem::size_of::<SECURITY_ATTRIBUTES>() == 12);
const _: () = assert!(core::mem::align_of::<SECURITY_ATTRIBUTES>() == 4);
const _: () = assert!(core::mem::offset_of!(SECURITY_ATTRIBUTES, nLength) == 0);
const _: () = assert!(core::mem::offset_of!(SECURITY_ATTRIBUTES, lpSecurityDescriptor) == 4);
const _: () = assert!(core::mem::offset_of!(SECURITY_ATTRIBUTES, bInheritHandle) == 8);
impl crate::GuestArg for SECURITY_ATTRIBUTES {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
