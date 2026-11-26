// Generated from pinned Windows.Win32.winmd, x86 only.
#![allow(non_snake_case)]
#[repr(C, align(4))]
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
#[repr(C, align(2))]
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
#[repr(C, align(4))]
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
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct POINTS {
    pub x: i16,
    pub y: i16,
}
const _: () = assert!(core::mem::size_of::<POINTS>() == 4);
const _: () = assert!(core::mem::align_of::<POINTS>() == 2);
const _: () = assert!(core::mem::offset_of!(POINTS, x) == 0);
const _: () = assert!(core::mem::offset_of!(POINTS, y) == 2);
impl crate::GuestArg for POINTS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
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
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TOUCHINPUT {
    pub x: i32,
    pub y: i32,
    pub hSource: u32,
    pub dwID: u32,
    pub dwFlags: u32,
    pub dwMask: u32,
    pub dwTime: u32,
    pub dwExtraInfo: u32,
    pub cxContact: u32,
    pub cyContact: u32,
}
const _: () = assert!(core::mem::size_of::<TOUCHINPUT>() == 40);
const _: () = assert!(core::mem::align_of::<TOUCHINPUT>() == 4);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, x) == 0);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, y) == 4);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, hSource) == 8);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, dwID) == 12);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, dwFlags) == 16);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, dwMask) == 20);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, dwTime) == 24);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, dwExtraInfo) == 28);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, cxContact) == 32);
const _: () = assert!(core::mem::offset_of!(TOUCHINPUT, cyContact) == 36);
impl crate::GuestArg for TOUCHINPUT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 10;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GESTUREINFO {
    pub cbSize: u32,
    pub dwFlags: u32,
    pub dwID: u32,
    pub hwndTarget: u32,
    pub ptsLocation: POINTS,
    pub dwInstanceID: u32,
    pub dwSequenceID: u32,
    pub _padding0: [u8; 4],
    pub ullArguments: u64,
    pub cbExtraArgs: u32,
    pub _padding1: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<GESTUREINFO>() == 48);
const _: () = assert!(core::mem::align_of::<GESTUREINFO>() == 8);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, cbSize) == 0);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, dwFlags) == 4);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, dwID) == 8);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, hwndTarget) == 12);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, ptsLocation) == 16);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, dwInstanceID) == 20);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, dwSequenceID) == 24);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, ullArguments) == 32);
const _: () = assert!(core::mem::offset_of!(GESTUREINFO, cbExtraArgs) == 40);
impl crate::GuestArg for GESTUREINFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 12;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(1))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BLENDFUNCTION {
    pub BlendOp: u8,
    pub BlendFlags: u8,
    pub SourceConstantAlpha: u8,
    pub AlphaFormat: u8,
}
const _: () = assert!(core::mem::size_of::<BLENDFUNCTION>() == 4);
const _: () = assert!(core::mem::align_of::<BLENDFUNCTION>() == 1);
const _: () = assert!(core::mem::offset_of!(BLENDFUNCTION, BlendOp) == 0);
const _: () = assert!(core::mem::offset_of!(BLENDFUNCTION, BlendFlags) == 1);
const _: () = assert!(core::mem::offset_of!(BLENDFUNCTION, SourceConstantAlpha) == 2);
const _: () = assert!(core::mem::offset_of!(BLENDFUNCTION, AlphaFormat) == 3);
impl crate::GuestArg for BLENDFUNCTION {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TRIVERTEX {
    pub x: i32,
    pub y: i32,
    pub Red: u16,
    pub Green: u16,
    pub Blue: u16,
    pub Alpha: u16,
}
const _: () = assert!(core::mem::size_of::<TRIVERTEX>() == 16);
const _: () = assert!(core::mem::align_of::<TRIVERTEX>() == 4);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, x) == 0);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, y) == 4);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, Red) == 8);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, Green) == 10);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, Blue) == 12);
const _: () = assert!(core::mem::offset_of!(TRIVERTEX, Alpha) == 14);
impl crate::GuestArg for TRIVERTEX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GRADIENT_RECT {
    pub UpperLeft: u32,
    pub LowerRight: u32,
}
const _: () = assert!(core::mem::size_of::<GRADIENT_RECT>() == 8);
const _: () = assert!(core::mem::align_of::<GRADIENT_RECT>() == 4);
const _: () = assert!(core::mem::offset_of!(GRADIENT_RECT, UpperLeft) == 0);
const _: () = assert!(core::mem::offset_of!(GRADIENT_RECT, LowerRight) == 4);
impl crate::GuestArg for GRADIENT_RECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GRADIENT_TRIANGLE {
    pub Vertex1: u32,
    pub Vertex2: u32,
    pub Vertex3: u32,
}
const _: () = assert!(core::mem::size_of::<GRADIENT_TRIANGLE>() == 12);
const _: () = assert!(core::mem::align_of::<GRADIENT_TRIANGLE>() == 4);
const _: () = assert!(core::mem::offset_of!(GRADIENT_TRIANGLE, Vertex1) == 0);
const _: () = assert!(core::mem::offset_of!(GRADIENT_TRIANGLE, Vertex2) == 4);
const _: () = assert!(core::mem::offset_of!(GRADIENT_TRIANGLE, Vertex3) == 8);
impl crate::GuestArg for GRADIENT_TRIANGLE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
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
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPRESENT_PARAMETERS {
    pub BackBufferWidth: u32,
    pub BackBufferHeight: u32,
    pub BackBufferFormat: u32,
    pub BackBufferCount: u32,
    pub MultiSampleType: i32,
    pub MultiSampleQuality: u32,
    pub SwapEffect: i32,
    pub hDeviceWindow: u32,
    pub Windowed: i32,
    pub EnableAutoDepthStencil: i32,
    pub AutoDepthStencilFormat: u32,
    pub Flags: u32,
    pub FullScreen_RefreshRateInHz: u32,
    pub PresentationInterval: u32,
}
const _: () = assert!(core::mem::size_of::<D3DPRESENT_PARAMETERS>() == 56);
const _: () = assert!(core::mem::align_of::<D3DPRESENT_PARAMETERS>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, BackBufferWidth) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, BackBufferHeight) == 4);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, BackBufferFormat) == 8);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, BackBufferCount) == 12);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, MultiSampleType) == 16);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, MultiSampleQuality) == 20);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, SwapEffect) == 24);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, hDeviceWindow) == 28);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, Windowed) == 32);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, EnableAutoDepthStencil) == 36);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, AutoDepthStencilFormat) == 40);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, Flags) == 44);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, FullScreen_RefreshRateInHz) == 48);
const _: () = assert!(core::mem::offset_of!(D3DPRESENT_PARAMETERS, PresentationInterval) == 52);
impl crate::GuestArg for D3DPRESENT_PARAMETERS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 14;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLOCKED_RECT {
    pub Pitch: i32,
    pub pBits: u32,
}
const _: () = assert!(core::mem::size_of::<D3DLOCKED_RECT>() == 8);
const _: () = assert!(core::mem::align_of::<D3DLOCKED_RECT>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DLOCKED_RECT, Pitch) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLOCKED_RECT, pBits) == 4);
impl crate::GuestArg for D3DLOCKED_RECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DSURFACE_DESC {
    pub Format: u32,
    pub Type: i32,
    pub Usage: u32,
    pub Pool: i32,
    pub MultiSampleType: i32,
    pub MultiSampleQuality: u32,
    pub Width: u32,
    pub Height: u32,
}
const _: () = assert!(core::mem::size_of::<D3DSURFACE_DESC>() == 32);
const _: () = assert!(core::mem::align_of::<D3DSURFACE_DESC>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Type) == 4);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Usage) == 8);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Pool) == 12);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, MultiSampleType) == 16);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, MultiSampleQuality) == 20);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Width) == 24);
const _: () = assert!(core::mem::offset_of!(D3DSURFACE_DESC, Height) == 28);
impl crate::GuestArg for D3DSURFACE_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVIEWPORT9 {
    pub X: u32,
    pub Y: u32,
    pub Width: u32,
    pub Height: u32,
    pub MinZ: f32,
    pub MaxZ: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVIEWPORT9>() == 24);
const _: () = assert!(core::mem::align_of::<D3DVIEWPORT9>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, X) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, Y) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, Width) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, Height) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, MinZ) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT9, MaxZ) == 20);
impl crate::GuestArg for D3DVIEWPORT9 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVERTEXBUFFER_DESC {
    pub Format: u32,
    pub Type: i32,
    pub Usage: u32,
    pub Pool: i32,
    pub Size: u32,
    pub FVF: u32,
}
const _: () = assert!(core::mem::size_of::<D3DVERTEXBUFFER_DESC>() == 24);
const _: () = assert!(core::mem::align_of::<D3DVERTEXBUFFER_DESC>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, Type) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, Usage) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, Pool) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, Size) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFER_DESC, FVF) == 20);
impl crate::GuestArg for D3DVERTEXBUFFER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DINDEXBUFFER_DESC {
    pub Format: u32,
    pub Type: i32,
    pub Usage: u32,
    pub Pool: i32,
    pub Size: u32,
}
const _: () = assert!(core::mem::size_of::<D3DINDEXBUFFER_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3DINDEXBUFFER_DESC>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DINDEXBUFFER_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3DINDEXBUFFER_DESC, Type) == 4);
const _: () = assert!(core::mem::offset_of!(D3DINDEXBUFFER_DESC, Usage) == 8);
const _: () = assert!(core::mem::offset_of!(D3DINDEXBUFFER_DESC, Pool) == 12);
const _: () = assert!(core::mem::offset_of!(D3DINDEXBUFFER_DESC, Size) == 16);
impl crate::GuestArg for D3DINDEXBUFFER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDISPLAYMODE {
    pub Width: u32,
    pub Height: u32,
    pub RefreshRate: u32,
    pub Format: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDISPLAYMODE>() == 16);
const _: () = assert!(core::mem::align_of::<D3DDISPLAYMODE>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DDISPLAYMODE, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDISPLAYMODE, Height) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDISPLAYMODE, RefreshRate) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDISPLAYMODE, Format) == 12);
impl crate::GuestArg for D3DDISPLAYMODE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDEVICE_CREATION_PARAMETERS {
    pub AdapterOrdinal: u32,
    pub DeviceType: i32,
    pub hFocusWindow: u32,
    pub BehaviorFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDEVICE_CREATION_PARAMETERS>() == 16);
const _: () = assert!(core::mem::align_of::<D3DDEVICE_CREATION_PARAMETERS>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVICE_CREATION_PARAMETERS, AdapterOrdinal) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDEVICE_CREATION_PARAMETERS, DeviceType) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVICE_CREATION_PARAMETERS, hFocusWindow) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDEVICE_CREATION_PARAMETERS, BehaviorFlags) == 12);
impl crate::GuestArg for D3DDEVICE_CREATION_PARAMETERS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVSHADERCAPS2_0 {
    pub Caps: u32,
    pub DynamicFlowControlDepth: i32,
    pub NumTemps: i32,
    pub StaticFlowControlDepth: i32,
}
const _: () = assert!(core::mem::size_of::<D3DVSHADERCAPS2_0>() == 16);
const _: () = assert!(core::mem::align_of::<D3DVSHADERCAPS2_0>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DVSHADERCAPS2_0, Caps) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVSHADERCAPS2_0, DynamicFlowControlDepth) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVSHADERCAPS2_0, NumTemps) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVSHADERCAPS2_0, StaticFlowControlDepth) == 12);
impl crate::GuestArg for D3DVSHADERCAPS2_0 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPSHADERCAPS2_0 {
    pub Caps: u32,
    pub DynamicFlowControlDepth: i32,
    pub NumTemps: i32,
    pub StaticFlowControlDepth: i32,
    pub NumInstructionSlots: i32,
}
const _: () = assert!(core::mem::size_of::<D3DPSHADERCAPS2_0>() == 20);
const _: () = assert!(core::mem::align_of::<D3DPSHADERCAPS2_0>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DPSHADERCAPS2_0, Caps) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPSHADERCAPS2_0, DynamicFlowControlDepth) == 4);
const _: () = assert!(core::mem::offset_of!(D3DPSHADERCAPS2_0, NumTemps) == 8);
const _: () = assert!(core::mem::offset_of!(D3DPSHADERCAPS2_0, StaticFlowControlDepth) == 12);
const _: () = assert!(core::mem::offset_of!(D3DPSHADERCAPS2_0, NumInstructionSlots) == 16);
impl crate::GuestArg for D3DPSHADERCAPS2_0 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DCAPS9 {
    pub DeviceType: i32,
    pub AdapterOrdinal: u32,
    pub Caps: u32,
    pub Caps2: u32,
    pub Caps3: u32,
    pub PresentationIntervals: u32,
    pub CursorCaps: u32,
    pub DevCaps: u32,
    pub PrimitiveMiscCaps: u32,
    pub RasterCaps: u32,
    pub ZCmpCaps: u32,
    pub SrcBlendCaps: u32,
    pub DestBlendCaps: u32,
    pub AlphaCmpCaps: u32,
    pub ShadeCaps: u32,
    pub TextureCaps: u32,
    pub TextureFilterCaps: u32,
    pub CubeTextureFilterCaps: u32,
    pub VolumeTextureFilterCaps: u32,
    pub TextureAddressCaps: u32,
    pub VolumeTextureAddressCaps: u32,
    pub LineCaps: u32,
    pub MaxTextureWidth: u32,
    pub MaxTextureHeight: u32,
    pub MaxVolumeExtent: u32,
    pub MaxTextureRepeat: u32,
    pub MaxTextureAspectRatio: u32,
    pub MaxAnisotropy: u32,
    pub MaxVertexW: f32,
    pub GuardBandLeft: f32,
    pub GuardBandTop: f32,
    pub GuardBandRight: f32,
    pub GuardBandBottom: f32,
    pub ExtentsAdjust: f32,
    pub StencilCaps: u32,
    pub FVFCaps: u32,
    pub TextureOpCaps: u32,
    pub MaxTextureBlendStages: u32,
    pub MaxSimultaneousTextures: u32,
    pub VertexProcessingCaps: u32,
    pub MaxActiveLights: u32,
    pub MaxUserClipPlanes: u32,
    pub MaxVertexBlendMatrices: u32,
    pub MaxVertexBlendMatrixIndex: u32,
    pub MaxPointSize: f32,
    pub MaxPrimitiveCount: u32,
    pub MaxVertexIndex: u32,
    pub MaxStreams: u32,
    pub MaxStreamStride: u32,
    pub VertexShaderVersion: u32,
    pub MaxVertexShaderConst: u32,
    pub PixelShaderVersion: u32,
    pub PixelShader1xMaxValue: f32,
    pub DevCaps2: u32,
    pub MaxNpatchTessellationLevel: f32,
    pub Reserved5: u32,
    pub MasterAdapterOrdinal: u32,
    pub AdapterOrdinalInGroup: u32,
    pub NumberOfAdaptersInGroup: u32,
    pub DeclTypes: u32,
    pub NumSimultaneousRTs: u32,
    pub StretchRectFilterCaps: u32,
    pub VS20Caps: D3DVSHADERCAPS2_0,
    pub PS20Caps: D3DPSHADERCAPS2_0,
    pub VertexTextureFilterCaps: u32,
    pub MaxVShaderInstructionsExecuted: u32,
    pub MaxPShaderInstructionsExecuted: u32,
    pub MaxVertexShader30InstructionSlots: u32,
    pub MaxPixelShader30InstructionSlots: u32,
}
const _: () = assert!(core::mem::size_of::<D3DCAPS9>() == 304);
const _: () = assert!(core::mem::align_of::<D3DCAPS9>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, DeviceType) == 0);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, AdapterOrdinal) == 4);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, Caps) == 8);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, Caps2) == 12);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, Caps3) == 16);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, PresentationIntervals) == 20);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, CursorCaps) == 24);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, DevCaps) == 28);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, PrimitiveMiscCaps) == 32);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, RasterCaps) == 36);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, ZCmpCaps) == 40);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, SrcBlendCaps) == 44);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, DestBlendCaps) == 48);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, AlphaCmpCaps) == 52);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, ShadeCaps) == 56);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, TextureCaps) == 60);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, TextureFilterCaps) == 64);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, CubeTextureFilterCaps) == 68);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VolumeTextureFilterCaps) == 72);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, TextureAddressCaps) == 76);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VolumeTextureAddressCaps) == 80);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, LineCaps) == 84);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxTextureWidth) == 88);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxTextureHeight) == 92);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVolumeExtent) == 96);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxTextureRepeat) == 100);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxTextureAspectRatio) == 104);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxAnisotropy) == 108);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexW) == 112);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, GuardBandLeft) == 116);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, GuardBandTop) == 120);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, GuardBandRight) == 124);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, GuardBandBottom) == 128);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, ExtentsAdjust) == 132);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, StencilCaps) == 136);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, FVFCaps) == 140);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, TextureOpCaps) == 144);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxTextureBlendStages) == 148);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxSimultaneousTextures) == 152);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VertexProcessingCaps) == 156);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxActiveLights) == 160);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxUserClipPlanes) == 164);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexBlendMatrices) == 168);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexBlendMatrixIndex) == 172);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxPointSize) == 176);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxPrimitiveCount) == 180);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexIndex) == 184);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxStreams) == 188);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxStreamStride) == 192);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VertexShaderVersion) == 196);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexShaderConst) == 200);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, PixelShaderVersion) == 204);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, PixelShader1xMaxValue) == 208);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, DevCaps2) == 212);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxNpatchTessellationLevel) == 216);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, Reserved5) == 220);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MasterAdapterOrdinal) == 224);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, AdapterOrdinalInGroup) == 228);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, NumberOfAdaptersInGroup) == 232);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, DeclTypes) == 236);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, NumSimultaneousRTs) == 240);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, StretchRectFilterCaps) == 244);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VS20Caps) == 248);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, PS20Caps) == 264);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, VertexTextureFilterCaps) == 284);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVShaderInstructionsExecuted) == 288);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxPShaderInstructionsExecuted) == 292);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxVertexShader30InstructionSlots) == 296);
const _: () = assert!(core::mem::offset_of!(D3DCAPS9, MaxPixelShader30InstructionSlots) == 300);
impl crate::GuestArg for D3DCAPS9 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 76;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, packed(4))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DADAPTER_IDENTIFIER9 {
    pub Driver: [i8; 512],
    pub Description: [i8; 512],
    pub DeviceName: [i8; 32],
    pub DriverVersion: i64,
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DeviceIdentifier: [u32; 4],
    pub WHQLLevel: u32,
}
const _: () = assert!(core::mem::size_of::<D3DADAPTER_IDENTIFIER9>() == 1100);
const _: () = assert!(core::mem::align_of::<D3DADAPTER_IDENTIFIER9>() == 4);
impl Default for D3DADAPTER_IDENTIFIER9 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, Driver) == 0);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, Description) == 512);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, DeviceName) == 1024);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, DriverVersion) == 1056);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, VendorId) == 1064);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, DeviceId) == 1068);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, SubSysId) == 1072);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, Revision) == 1076);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, DeviceIdentifier) == 1080);
const _: () = assert!(core::mem::offset_of!(D3DADAPTER_IDENTIFIER9, WHQLLevel) == 1096);
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DDDEVICEIDENTIFIER2 {
    pub szDriver: [i8; 512],
    pub szDescription: [i8; 512],
    pub liDriverVersion: i64,
    pub dwVendorId: u32,
    pub dwDeviceId: u32,
    pub dwSubSysId: u32,
    pub dwRevision: u32,
    pub guidDeviceIdentifier: [u32; 4],
    pub dwWHQLLevel: u32,
    pub _padding0: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<DDDEVICEIDENTIFIER2>() == 1072);
const _: () = assert!(core::mem::align_of::<DDDEVICEIDENTIFIER2>() == 8);
impl Default for DDDEVICEIDENTIFIER2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, szDriver) == 0);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, szDescription) == 512);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, liDriverVersion) == 1024);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, dwVendorId) == 1032);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, dwDeviceId) == 1036);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, dwSubSysId) == 1040);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, dwRevision) == 1044);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, guidDeviceIdentifier) == 1048);
const _: () = assert!(core::mem::offset_of!(DDDEVICEIDENTIFIER2, dwWHQLLevel) == 1064);
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct STATSTG {
    pub pwcsName: u32,
    pub r#type: u32,
    pub cbSize: u64,
    pub mtime: FILETIME,
    pub ctime: FILETIME,
    pub atime: FILETIME,
    pub grfMode: u32,
    pub grfLocksSupported: u32,
    pub clsid: [u32; 4],
    pub grfStateBits: u32,
    pub reserved: u32,
}
const _: () = assert!(core::mem::size_of::<STATSTG>() == 72);
const _: () = assert!(core::mem::align_of::<STATSTG>() == 8);
const _: () = assert!(core::mem::offset_of!(STATSTG, pwcsName) == 0);
const _: () = assert!(core::mem::offset_of!(STATSTG, r#type) == 4);
const _: () = assert!(core::mem::offset_of!(STATSTG, cbSize) == 8);
const _: () = assert!(core::mem::offset_of!(STATSTG, mtime) == 16);
const _: () = assert!(core::mem::offset_of!(STATSTG, ctime) == 24);
const _: () = assert!(core::mem::offset_of!(STATSTG, atime) == 32);
const _: () = assert!(core::mem::offset_of!(STATSTG, grfMode) == 40);
const _: () = assert!(core::mem::offset_of!(STATSTG, grfLocksSupported) == 44);
const _: () = assert!(core::mem::offset_of!(STATSTG, clsid) == 48);
const _: () = assert!(core::mem::offset_of!(STATSTG, grfStateBits) == 64);
const _: () = assert!(core::mem::offset_of!(STATSTG, reserved) == 68);
impl crate::GuestArg for STATSTG {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 18;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SAFEARRAYBOUND {
    pub cElements: u32,
    pub lLbound: i32,
}
const _: () = assert!(core::mem::size_of::<SAFEARRAYBOUND>() == 8);
const _: () = assert!(core::mem::align_of::<SAFEARRAYBOUND>() == 4);
const _: () = assert!(core::mem::offset_of!(SAFEARRAYBOUND, cElements) == 0);
const _: () = assert!(core::mem::offset_of!(SAFEARRAYBOUND, lLbound) == 4);
impl crate::GuestArg for SAFEARRAYBOUND {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SAFEARRAY {
    pub cDims: u16,
    pub fFeatures: u16,
    pub cbElements: u32,
    pub cLocks: u32,
    pub pvData: u32,
    pub rgsabound: [SAFEARRAYBOUND; 1],
}
const _: () = assert!(core::mem::size_of::<SAFEARRAY>() == 24);
const _: () = assert!(core::mem::align_of::<SAFEARRAY>() == 4);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, cDims) == 0);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, fFeatures) == 2);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, cbElements) == 4);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, cLocks) == 8);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, pvData) == 12);
const _: () = assert!(core::mem::offset_of!(SAFEARRAY, rgsabound) == 16);
impl crate::GuestArg for SAFEARRAY {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TYPEDESC {
    pub Anonymous: [u8; 4],
    pub vt: u16,
    pub _padding0: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<TYPEDESC>() == 8);
const _: () = assert!(core::mem::align_of::<TYPEDESC>() == 4);
const _: () = assert!(core::mem::offset_of!(TYPEDESC, Anonymous) == 0);
const _: () = assert!(core::mem::offset_of!(TYPEDESC, vt) == 4);
impl crate::GuestArg for TYPEDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ELEMDESC {
    pub tdesc: TYPEDESC,
    pub Anonymous: [u8; 8],
}
const _: () = assert!(core::mem::size_of::<ELEMDESC>() == 16);
const _: () = assert!(core::mem::align_of::<ELEMDESC>() == 4);
const _: () = assert!(core::mem::offset_of!(ELEMDESC, tdesc) == 0);
const _: () = assert!(core::mem::offset_of!(ELEMDESC, Anonymous) == 8);
impl crate::GuestArg for ELEMDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FUNCDESC {
    pub memid: i32,
    pub lprgscode: u32,
    pub lprgelemdescParam: u32,
    pub funckind: i32,
    pub invkind: i32,
    pub callconv: i32,
    pub cParams: i16,
    pub cParamsOpt: i16,
    pub oVft: i16,
    pub cScodes: i16,
    pub elemdescFunc: ELEMDESC,
    pub wFuncFlags: u16,
    pub _padding0: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<FUNCDESC>() == 52);
const _: () = assert!(core::mem::align_of::<FUNCDESC>() == 4);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, memid) == 0);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, lprgscode) == 4);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, lprgelemdescParam) == 8);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, funckind) == 12);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, invkind) == 16);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, callconv) == 20);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, cParams) == 24);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, cParamsOpt) == 26);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, oVft) == 28);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, cScodes) == 30);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, elemdescFunc) == 32);
const _: () = assert!(core::mem::offset_of!(FUNCDESC, wFuncFlags) == 48);
impl crate::GuestArg for FUNCDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VARDESC {
    pub memid: i32,
    pub lpstrSchema: u32,
    pub Anonymous: [u8; 4],
    pub elemdescVar: ELEMDESC,
    pub wVarFlags: u16,
    pub _padding0: [u8; 2],
    pub varkind: i32,
}
const _: () = assert!(core::mem::size_of::<VARDESC>() == 36);
const _: () = assert!(core::mem::align_of::<VARDESC>() == 4);
const _: () = assert!(core::mem::offset_of!(VARDESC, memid) == 0);
const _: () = assert!(core::mem::offset_of!(VARDESC, lpstrSchema) == 4);
const _: () = assert!(core::mem::offset_of!(VARDESC, Anonymous) == 8);
const _: () = assert!(core::mem::offset_of!(VARDESC, elemdescVar) == 12);
const _: () = assert!(core::mem::offset_of!(VARDESC, wVarFlags) == 28);
const _: () = assert!(core::mem::offset_of!(VARDESC, varkind) == 32);
impl crate::GuestArg for VARDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 9;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TYPEATTR {
    pub guid: [u32; 4],
    pub lcid: u32,
    pub dwReserved: u32,
    pub memidConstructor: i32,
    pub memidDestructor: i32,
    pub lpstrSchema: u32,
    pub cbSizeInstance: u32,
    pub typekind: i32,
    pub cFuncs: u16,
    pub cVars: u16,
    pub cImplTypes: u16,
    pub cbSizeVft: u16,
    pub cbAlignment: u16,
    pub wTypeFlags: u16,
    pub wMajorVerNum: u16,
    pub wMinorVerNum: u16,
    pub tdescAlias: TYPEDESC,
    pub idldescType: [u8; 8],
}
const _: () = assert!(core::mem::size_of::<TYPEATTR>() == 76);
const _: () = assert!(core::mem::align_of::<TYPEATTR>() == 4);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, guid) == 0);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, lcid) == 16);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, dwReserved) == 20);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, memidConstructor) == 24);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, memidDestructor) == 28);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, lpstrSchema) == 32);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cbSizeInstance) == 36);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, typekind) == 40);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cFuncs) == 44);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cVars) == 46);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cImplTypes) == 48);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cbSizeVft) == 50);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, cbAlignment) == 52);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, wTypeFlags) == 54);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, wMajorVerNum) == 56);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, wMinorVerNum) == 58);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, tdescAlias) == 60);
const _: () = assert!(core::mem::offset_of!(TYPEATTR, idldescType) == 68);
impl crate::GuestArg for TYPEATTR {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 19;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TLIBATTR {
    pub guid: [u32; 4],
    pub lcid: u32,
    pub syskind: i32,
    pub wMajorVerNum: u16,
    pub wMinorVerNum: u16,
    pub wLibFlags: u16,
    pub _padding0: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<TLIBATTR>() == 32);
const _: () = assert!(core::mem::align_of::<TLIBATTR>() == 4);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, guid) == 0);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, lcid) == 16);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, syskind) == 20);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, wMajorVerNum) == 24);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, wMinorVerNum) == 26);
const _: () = assert!(core::mem::offset_of!(TLIBATTR, wLibFlags) == 28);
impl crate::GuestArg for TLIBATTR {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FONTDESC {
    pub cbSizeofstruct: u32,
    pub lpstrName: u32,
    pub cySize: [u8; 8],
    pub sWeight: i16,
    pub sCharset: i16,
    pub fItalic: i32,
    pub fUnderline: i32,
    pub fStrikethrough: i32,
}
const _: () = assert!(core::mem::size_of::<FONTDESC>() == 32);
const _: () = assert!(core::mem::align_of::<FONTDESC>() == 8);
const _: () = assert!(core::mem::offset_of!(FONTDESC, cbSizeofstruct) == 0);
const _: () = assert!(core::mem::offset_of!(FONTDESC, lpstrName) == 4);
const _: () = assert!(core::mem::offset_of!(FONTDESC, cySize) == 8);
const _: () = assert!(core::mem::offset_of!(FONTDESC, sWeight) == 16);
const _: () = assert!(core::mem::offset_of!(FONTDESC, sCharset) == 18);
const _: () = assert!(core::mem::offset_of!(FONTDESC, fItalic) == 20);
const _: () = assert!(core::mem::offset_of!(FONTDESC, fUnderline) == 24);
const _: () = assert!(core::mem::offset_of!(FONTDESC, fStrikethrough) == 28);
impl crate::GuestArg for FONTDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PICTDESC {
    pub cbSizeofstruct: u32,
    pub picType: u32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<PICTDESC>() == 20);
const _: () = assert!(core::mem::align_of::<PICTDESC>() == 4);
const _: () = assert!(core::mem::offset_of!(PICTDESC, cbSizeofstruct) == 0);
const _: () = assert!(core::mem::offset_of!(PICTDESC, picType) == 4);
const _: () = assert!(core::mem::offset_of!(PICTDESC, Anonymous) == 8);
impl crate::GuestArg for PICTDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MULTI_QI {
    pub pIID: u32,
    pub pItf: u32,
    pub hr: i32,
}
const _: () = assert!(core::mem::size_of::<MULTI_QI>() == 12);
const _: () = assert!(core::mem::align_of::<MULTI_QI>() == 4);
const _: () = assert!(core::mem::offset_of!(MULTI_QI, pIID) == 0);
const _: () = assert!(core::mem::offset_of!(MULTI_QI, pItf) == 4);
const _: () = assert!(core::mem::offset_of!(MULTI_QI, hr) == 8);
impl crate::GuestArg for MULTI_QI {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct COSERVERINFO {
    pub dwReserved1: u32,
    pub pwszName: u32,
    pub pAuthInfo: u32,
    pub dwReserved2: u32,
}
const _: () = assert!(core::mem::size_of::<COSERVERINFO>() == 16);
const _: () = assert!(core::mem::align_of::<COSERVERINFO>() == 4);
const _: () = assert!(core::mem::offset_of!(COSERVERINFO, dwReserved1) == 0);
const _: () = assert!(core::mem::offset_of!(COSERVERINFO, pwszName) == 4);
const _: () = assert!(core::mem::offset_of!(COSERVERINFO, pAuthInfo) == 8);
const _: () = assert!(core::mem::offset_of!(COSERVERINFO, dwReserved2) == 12);
impl crate::GuestArg for COSERVERINFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
// Graphics records.
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BLEND_DESC {
    pub AlphaToCoverageEnable: i32,
    pub BlendEnable: [i32; 8],
    pub SrcBlend: i32,
    pub DestBlend: i32,
    pub BlendOp: i32,
    pub SrcBlendAlpha: i32,
    pub DestBlendAlpha: i32,
    pub BlendOpAlpha: i32,
    pub RenderTargetWriteMask: [u8; 8],
}
const _: () = assert!(core::mem::size_of::<D3D10_BLEND_DESC>() == 68);
const _: () = assert!(core::mem::align_of::<D3D10_BLEND_DESC>() == 4);
impl Default for D3D10_BLEND_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, AlphaToCoverageEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, BlendEnable) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, SrcBlend) == 36);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, DestBlend) == 40);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, BlendOp) == 44);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, SrcBlendAlpha) == 48);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, DestBlendAlpha) == 52);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, BlendOpAlpha) == 56);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC, RenderTargetWriteMask) == 60);
impl crate::GuestArg for D3D10_BLEND_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 17;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BLEND_DESC1 {
    pub AlphaToCoverageEnable: i32,
    pub IndependentBlendEnable: i32,
    pub RenderTarget: [D3D10_RENDER_TARGET_BLEND_DESC1; 8],
}
const _: () = assert!(core::mem::size_of::<D3D10_BLEND_DESC1>() == 264);
const _: () = assert!(core::mem::align_of::<D3D10_BLEND_DESC1>() == 4);
impl Default for D3D10_BLEND_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC1, AlphaToCoverageEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC1, IndependentBlendEnable) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_BLEND_DESC1, RenderTarget) == 8);
impl crate::GuestArg for D3D10_BLEND_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 66;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BOX {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_BOX>() == 24);
const _: () = assert!(core::mem::align_of::<D3D10_BOX>() == 4);
impl Default for D3D10_BOX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, left) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, top) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, front) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, right) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, bottom) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_BOX, back) == 20);
impl crate::GuestArg for D3D10_BOX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BUFFER_DESC {
    pub ByteWidth: u32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_BUFFER_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3D10_BUFFER_DESC>() == 4);
impl Default for D3D10_BUFFER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_DESC, ByteWidth) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_DESC, Usage) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_DESC, BindFlags) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_DESC, CPUAccessFlags) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_DESC, MiscFlags) == 16);
impl crate::GuestArg for D3D10_BUFFER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BUFFER_RTV {
    pub FirstElement: u32,
    pub NumElements: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_BUFFER_RTV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_BUFFER_RTV>() == 4);
impl Default for D3D10_BUFFER_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_RTV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_RTV, NumElements) == 4);
impl crate::GuestArg for D3D10_BUFFER_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_BUFFER_SRV {
    pub FirstElement: u32,
    pub NumElements: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_BUFFER_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_BUFFER_SRV>() == 4);
impl Default for D3D10_BUFFER_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_SRV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_BUFFER_SRV, NumElements) == 4);
impl crate::GuestArg for D3D10_BUFFER_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_COUNTER_DESC {
    pub Counter: i32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_COUNTER_DESC>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_COUNTER_DESC>() == 4);
impl Default for D3D10_COUNTER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_COUNTER_DESC, Counter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_COUNTER_DESC, MiscFlags) == 4);
impl crate::GuestArg for D3D10_COUNTER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_COUNTER_INFO {
    pub LastDeviceDependentCounter: i32,
    pub NumSimultaneousCounters: u32,
    pub NumDetectableParallelUnits: u8,
    pub _padding0: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<D3D10_COUNTER_INFO>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_COUNTER_INFO>() == 4);
impl Default for D3D10_COUNTER_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_COUNTER_INFO, LastDeviceDependentCounter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_COUNTER_INFO, NumSimultaneousCounters) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_COUNTER_INFO, NumDetectableParallelUnits) == 8);
impl crate::GuestArg for D3D10_COUNTER_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_DEPTH_STENCILOP_DESC {
    pub StencilFailOp: i32,
    pub StencilDepthFailOp: i32,
    pub StencilPassOp: i32,
    pub StencilFunc: i32,
}
const _: () = assert!(core::mem::size_of::<D3D10_DEPTH_STENCILOP_DESC>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_DEPTH_STENCILOP_DESC>() == 4);
impl Default for D3D10_DEPTH_STENCILOP_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCILOP_DESC, StencilFailOp) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCILOP_DESC, StencilDepthFailOp) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCILOP_DESC, StencilPassOp) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCILOP_DESC, StencilFunc) == 12);
impl crate::GuestArg for D3D10_DEPTH_STENCILOP_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_DEPTH_STENCIL_DESC {
    pub DepthEnable: i32,
    pub DepthWriteMask: i32,
    pub DepthFunc: i32,
    pub StencilEnable: i32,
    pub StencilReadMask: u8,
    pub StencilWriteMask: u8,
    pub _padding0: [u8; 2],
    pub FrontFace: D3D10_DEPTH_STENCILOP_DESC,
    pub BackFace: D3D10_DEPTH_STENCILOP_DESC,
}
const _: () = assert!(core::mem::size_of::<D3D10_DEPTH_STENCIL_DESC>() == 52);
const _: () = assert!(core::mem::align_of::<D3D10_DEPTH_STENCIL_DESC>() == 4);
impl Default for D3D10_DEPTH_STENCIL_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, DepthEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, DepthWriteMask) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, DepthFunc) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, StencilEnable) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, StencilReadMask) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, StencilWriteMask) == 17);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, FrontFace) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_DESC, BackFace) == 36);
impl crate::GuestArg for D3D10_DEPTH_STENCIL_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_DEPTH_STENCIL_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<D3D10_DEPTH_STENCIL_VIEW_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3D10_DEPTH_STENCIL_VIEW_DESC>() == 4);
impl Default for D3D10_DEPTH_STENCIL_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_DEPTH_STENCIL_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D10_DEPTH_STENCIL_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_INPUT_ELEMENT_DESC {
    pub SemanticName: u32,
    pub SemanticIndex: u32,
    pub Format: i32,
    pub InputSlot: u32,
    pub AlignedByteOffset: u32,
    pub InputSlotClass: i32,
    pub InstanceDataStepRate: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_INPUT_ELEMENT_DESC>() == 28);
const _: () = assert!(core::mem::align_of::<D3D10_INPUT_ELEMENT_DESC>() == 4);
impl Default for D3D10_INPUT_ELEMENT_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, SemanticName) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, SemanticIndex) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, Format) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, InputSlot) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, AlignedByteOffset) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, InputSlotClass) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_INPUT_ELEMENT_DESC, InstanceDataStepRate) == 24);
impl crate::GuestArg for D3D10_INPUT_ELEMENT_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 7;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_MAPPED_TEXTURE2D {
    pub pData: u32,
    pub RowPitch: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_MAPPED_TEXTURE2D>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_MAPPED_TEXTURE2D>() == 4);
impl Default for D3D10_MAPPED_TEXTURE2D { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_MAPPED_TEXTURE2D, pData) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_MAPPED_TEXTURE2D, RowPitch) == 4);
impl crate::GuestArg for D3D10_MAPPED_TEXTURE2D {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_MAPPED_TEXTURE3D {
    pub pData: u32,
    pub RowPitch: u32,
    pub DepthPitch: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_MAPPED_TEXTURE3D>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_MAPPED_TEXTURE3D>() == 4);
impl Default for D3D10_MAPPED_TEXTURE3D { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_MAPPED_TEXTURE3D, pData) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_MAPPED_TEXTURE3D, RowPitch) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_MAPPED_TEXTURE3D, DepthPitch) == 8);
impl crate::GuestArg for D3D10_MAPPED_TEXTURE3D {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_QUERY_DATA_PIPELINE_STATISTICS {
    pub IAVertices: u64,
    pub IAPrimitives: u64,
    pub VSInvocations: u64,
    pub GSInvocations: u64,
    pub GSPrimitives: u64,
    pub CInvocations: u64,
    pub CPrimitives: u64,
    pub PSInvocations: u64,
}
const _: () = assert!(core::mem::size_of::<D3D10_QUERY_DATA_PIPELINE_STATISTICS>() == 64);
const _: () = assert!(core::mem::align_of::<D3D10_QUERY_DATA_PIPELINE_STATISTICS>() == 8);
impl Default for D3D10_QUERY_DATA_PIPELINE_STATISTICS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, IAVertices) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, IAPrimitives) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, VSInvocations) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, GSInvocations) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, GSPrimitives) == 32);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, CInvocations) == 40);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, CPrimitives) == 48);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_PIPELINE_STATISTICS, PSInvocations) == 56);
impl crate::GuestArg for D3D10_QUERY_DATA_PIPELINE_STATISTICS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 16;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_QUERY_DATA_SO_STATISTICS {
    pub NumPrimitivesWritten: u64,
    pub PrimitivesStorageNeeded: u64,
}
const _: () = assert!(core::mem::size_of::<D3D10_QUERY_DATA_SO_STATISTICS>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_QUERY_DATA_SO_STATISTICS>() == 8);
impl Default for D3D10_QUERY_DATA_SO_STATISTICS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_SO_STATISTICS, NumPrimitivesWritten) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_SO_STATISTICS, PrimitivesStorageNeeded) == 8);
impl crate::GuestArg for D3D10_QUERY_DATA_SO_STATISTICS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_QUERY_DATA_TIMESTAMP_DISJOINT {
    pub Frequency: u64,
    pub Disjoint: i32,
    pub _padding0: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<D3D10_QUERY_DATA_TIMESTAMP_DISJOINT>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_QUERY_DATA_TIMESTAMP_DISJOINT>() == 8);
impl Default for D3D10_QUERY_DATA_TIMESTAMP_DISJOINT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_TIMESTAMP_DISJOINT, Frequency) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DATA_TIMESTAMP_DISJOINT, Disjoint) == 8);
impl crate::GuestArg for D3D10_QUERY_DATA_TIMESTAMP_DISJOINT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_QUERY_DESC {
    pub Query: i32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_QUERY_DESC>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_QUERY_DESC>() == 4);
impl Default for D3D10_QUERY_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DESC, Query) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_QUERY_DESC, MiscFlags) == 4);
impl crate::GuestArg for D3D10_QUERY_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_RASTERIZER_DESC {
    pub FillMode: i32,
    pub CullMode: i32,
    pub FrontCounterClockwise: i32,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: i32,
    pub ScissorEnable: i32,
    pub MultisampleEnable: i32,
    pub AntialiasedLineEnable: i32,
}
const _: () = assert!(core::mem::size_of::<D3D10_RASTERIZER_DESC>() == 40);
const _: () = assert!(core::mem::align_of::<D3D10_RASTERIZER_DESC>() == 4);
impl Default for D3D10_RASTERIZER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, FillMode) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, CullMode) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, FrontCounterClockwise) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, DepthBias) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, DepthBiasClamp) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, SlopeScaledDepthBias) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, DepthClipEnable) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, ScissorEnable) == 28);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, MultisampleEnable) == 32);
const _: () = assert!(core::mem::offset_of!(D3D10_RASTERIZER_DESC, AntialiasedLineEnable) == 36);
impl crate::GuestArg for D3D10_RASTERIZER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 10;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_RENDER_TARGET_BLEND_DESC1 {
    pub BlendEnable: i32,
    pub SrcBlend: i32,
    pub DestBlend: i32,
    pub BlendOp: i32,
    pub SrcBlendAlpha: i32,
    pub DestBlendAlpha: i32,
    pub BlendOpAlpha: i32,
    pub RenderTargetWriteMask: u8,
    pub _padding0: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<D3D10_RENDER_TARGET_BLEND_DESC1>() == 32);
const _: () = assert!(core::mem::align_of::<D3D10_RENDER_TARGET_BLEND_DESC1>() == 4);
impl Default for D3D10_RENDER_TARGET_BLEND_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, BlendEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, SrcBlend) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, DestBlend) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, BlendOp) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, SrcBlendAlpha) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, DestBlendAlpha) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, BlendOpAlpha) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_BLEND_DESC1, RenderTargetWriteMask) == 28);
impl crate::GuestArg for D3D10_RENDER_TARGET_BLEND_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_RENDER_TARGET_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<D3D10_RENDER_TARGET_VIEW_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3D10_RENDER_TARGET_VIEW_DESC>() == 4);
impl Default for D3D10_RENDER_TARGET_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_RENDER_TARGET_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D10_RENDER_TARGET_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_SAMPLER_DESC {
    pub Filter: i32,
    pub AddressU: i32,
    pub AddressV: i32,
    pub AddressW: i32,
    pub MipLODBias: f32,
    pub MaxAnisotropy: u32,
    pub ComparisonFunc: i32,
    pub BorderColor: [f32; 4],
    pub MinLOD: f32,
    pub MaxLOD: f32,
}
const _: () = assert!(core::mem::size_of::<D3D10_SAMPLER_DESC>() == 52);
const _: () = assert!(core::mem::align_of::<D3D10_SAMPLER_DESC>() == 4);
impl Default for D3D10_SAMPLER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, Filter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, AddressU) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, AddressV) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, AddressW) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, MipLODBias) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, MaxAnisotropy) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, ComparisonFunc) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, BorderColor) == 28);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, MinLOD) == 44);
const _: () = assert!(core::mem::offset_of!(D3D10_SAMPLER_DESC, MaxLOD) == 48);
impl crate::GuestArg for D3D10_SAMPLER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_SHADER_RESOURCE_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 16],
}
const _: () = assert!(core::mem::size_of::<D3D10_SHADER_RESOURCE_VIEW_DESC>() == 24);
const _: () = assert!(core::mem::align_of::<D3D10_SHADER_RESOURCE_VIEW_DESC>() == 4);
impl Default for D3D10_SHADER_RESOURCE_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D10_SHADER_RESOURCE_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_SHADER_RESOURCE_VIEW_DESC1 {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 16],
}
const _: () = assert!(core::mem::size_of::<D3D10_SHADER_RESOURCE_VIEW_DESC1>() == 24);
const _: () = assert!(core::mem::align_of::<D3D10_SHADER_RESOURCE_VIEW_DESC1>() == 4);
impl Default for D3D10_SHADER_RESOURCE_VIEW_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC1, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC1, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_SHADER_RESOURCE_VIEW_DESC1, Anonymous) == 8);
impl crate::GuestArg for D3D10_SHADER_RESOURCE_VIEW_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_SO_DECLARATION_ENTRY {
    pub SemanticName: u32,
    pub SemanticIndex: u32,
    pub StartComponent: u8,
    pub ComponentCount: u8,
    pub OutputSlot: u8,
    pub _padding0: [u8; 1],
}
const _: () = assert!(core::mem::size_of::<D3D10_SO_DECLARATION_ENTRY>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_SO_DECLARATION_ENTRY>() == 4);
impl Default for D3D10_SO_DECLARATION_ENTRY { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_SO_DECLARATION_ENTRY, SemanticName) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_SO_DECLARATION_ENTRY, SemanticIndex) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_SO_DECLARATION_ENTRY, StartComponent) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_SO_DECLARATION_ENTRY, ComponentCount) == 9);
const _: () = assert!(core::mem::offset_of!(D3D10_SO_DECLARATION_ENTRY, OutputSlot) == 10);
impl crate::GuestArg for D3D10_SO_DECLARATION_ENTRY {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_SUBRESOURCE_DATA {
    pub pSysMem: u32,
    pub SysMemPitch: u32,
    pub SysMemSlicePitch: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_SUBRESOURCE_DATA>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_SUBRESOURCE_DATA>() == 4);
impl Default for D3D10_SUBRESOURCE_DATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_SUBRESOURCE_DATA, pSysMem) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_SUBRESOURCE_DATA, SysMemPitch) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_SUBRESOURCE_DATA, SysMemSlicePitch) == 8);
impl crate::GuestArg for D3D10_SUBRESOURCE_DATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_ARRAY_DSV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_ARRAY_DSV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_ARRAY_DSV>() == 4);
impl Default for D3D10_TEX1D_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_DSV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_DSV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_DSV, ArraySize) == 8);
impl crate::GuestArg for D3D10_TEX1D_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_ARRAY_RTV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_ARRAY_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_ARRAY_RTV>() == 4);
impl Default for D3D10_TEX1D_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_RTV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_RTV, ArraySize) == 8);
impl crate::GuestArg for D3D10_TEX1D_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_ARRAY_SRV>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_ARRAY_SRV>() == 4);
impl Default for D3D10_TEX1D_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_SRV, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_SRV, FirstArraySlice) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_ARRAY_SRV, ArraySize) == 12);
impl crate::GuestArg for D3D10_TEX1D_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_DSV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_DSV>() == 4);
impl Default for D3D10_TEX1D_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_DSV, MipSlice) == 0);
impl crate::GuestArg for D3D10_TEX1D_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_RTV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_RTV>() == 4);
impl Default for D3D10_TEX1D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_RTV, MipSlice) == 0);
impl crate::GuestArg for D3D10_TEX1D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX1D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX1D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX1D_SRV>() == 4);
impl Default for D3D10_TEX1D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX1D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D10_TEX1D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_ARRAY_DSV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_ARRAY_DSV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_ARRAY_DSV>() == 4);
impl Default for D3D10_TEX2DMS_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_DSV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_DSV, ArraySize) == 4);
impl crate::GuestArg for D3D10_TEX2DMS_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_ARRAY_RTV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_ARRAY_RTV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_ARRAY_RTV>() == 4);
impl Default for D3D10_TEX2DMS_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_RTV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_RTV, ArraySize) == 4);
impl crate::GuestArg for D3D10_TEX2DMS_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_ARRAY_SRV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_ARRAY_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_ARRAY_SRV>() == 4);
impl Default for D3D10_TEX2DMS_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_SRV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_ARRAY_SRV, ArraySize) == 4);
impl crate::GuestArg for D3D10_TEX2DMS_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_DSV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_DSV>() == 4);
impl Default for D3D10_TEX2DMS_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_DSV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D10_TEX2DMS_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_RTV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_RTV>() == 4);
impl Default for D3D10_TEX2DMS_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_RTV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D10_TEX2DMS_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2DMS_SRV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2DMS_SRV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2DMS_SRV>() == 4);
impl Default for D3D10_TEX2DMS_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2DMS_SRV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D10_TEX2DMS_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_ARRAY_DSV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_ARRAY_DSV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_ARRAY_DSV>() == 4);
impl Default for D3D10_TEX2D_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_DSV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_DSV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_DSV, ArraySize) == 8);
impl crate::GuestArg for D3D10_TEX2D_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_ARRAY_RTV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_ARRAY_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_ARRAY_RTV>() == 4);
impl Default for D3D10_TEX2D_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_RTV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_RTV, ArraySize) == 8);
impl crate::GuestArg for D3D10_TEX2D_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_ARRAY_SRV>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_ARRAY_SRV>() == 4);
impl Default for D3D10_TEX2D_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_SRV, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_SRV, FirstArraySlice) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_ARRAY_SRV, ArraySize) == 12);
impl crate::GuestArg for D3D10_TEX2D_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_DSV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_DSV>() == 4);
impl Default for D3D10_TEX2D_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_DSV, MipSlice) == 0);
impl crate::GuestArg for D3D10_TEX2D_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_RTV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_RTV>() == 4);
impl Default for D3D10_TEX2D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_RTV, MipSlice) == 0);
impl crate::GuestArg for D3D10_TEX2D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX2D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX2D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX2D_SRV>() == 4);
impl Default for D3D10_TEX2D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX2D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D10_TEX2D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX3D_RTV {
    pub MipSlice: u32,
    pub FirstWSlice: u32,
    pub WSize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX3D_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D10_TEX3D_RTV>() == 4);
impl Default for D3D10_TEX3D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX3D_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX3D_RTV, FirstWSlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX3D_RTV, WSize) == 8);
impl crate::GuestArg for D3D10_TEX3D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEX3D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEX3D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEX3D_SRV>() == 4);
impl Default for D3D10_TEX3D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEX3D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEX3D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D10_TEX3D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEXCUBE_ARRAY_SRV1 {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub First2DArrayFace: u32,
    pub NumCubes: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEXCUBE_ARRAY_SRV1>() == 16);
const _: () = assert!(core::mem::align_of::<D3D10_TEXCUBE_ARRAY_SRV1>() == 4);
impl Default for D3D10_TEXCUBE_ARRAY_SRV1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_ARRAY_SRV1, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_ARRAY_SRV1, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_ARRAY_SRV1, First2DArrayFace) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_ARRAY_SRV1, NumCubes) == 12);
impl crate::GuestArg for D3D10_TEXCUBE_ARRAY_SRV1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEXCUBE_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEXCUBE_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D10_TEXCUBE_SRV>() == 4);
impl Default for D3D10_TEXCUBE_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXCUBE_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D10_TEXCUBE_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEXTURE1D_DESC {
    pub Width: u32,
    pub MipLevels: u32,
    pub ArraySize: u32,
    pub Format: i32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEXTURE1D_DESC>() == 32);
const _: () = assert!(core::mem::align_of::<D3D10_TEXTURE1D_DESC>() == 4);
impl Default for D3D10_TEXTURE1D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, ArraySize) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, Format) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, Usage) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, BindFlags) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, CPUAccessFlags) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE1D_DESC, MiscFlags) == 28);
impl crate::GuestArg for D3D10_TEXTURE1D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEXTURE2D_DESC {
    pub Width: u32,
    pub Height: u32,
    pub MipLevels: u32,
    pub ArraySize: u32,
    pub Format: i32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEXTURE2D_DESC>() == 44);
const _: () = assert!(core::mem::align_of::<D3D10_TEXTURE2D_DESC>() == 4);
impl Default for D3D10_TEXTURE2D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, MipLevels) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, ArraySize) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, Format) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, SampleDesc) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, Usage) == 28);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, BindFlags) == 32);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, CPUAccessFlags) == 36);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE2D_DESC, MiscFlags) == 40);
impl crate::GuestArg for D3D10_TEXTURE2D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_TEXTURE3D_DESC {
    pub Width: u32,
    pub Height: u32,
    pub Depth: u32,
    pub MipLevels: u32,
    pub Format: i32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D10_TEXTURE3D_DESC>() == 36);
const _: () = assert!(core::mem::align_of::<D3D10_TEXTURE3D_DESC>() == 4);
impl Default for D3D10_TEXTURE3D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, Depth) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, MipLevels) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, Format) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, Usage) == 20);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, BindFlags) == 24);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, CPUAccessFlags) == 28);
const _: () = assert!(core::mem::offset_of!(D3D10_TEXTURE3D_DESC, MiscFlags) == 32);
impl crate::GuestArg for D3D10_TEXTURE3D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 9;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D10_VIEWPORT {
    pub TopLeftX: i32,
    pub TopLeftY: i32,
    pub Width: u32,
    pub Height: u32,
    pub MinDepth: f32,
    pub MaxDepth: f32,
}
const _: () = assert!(core::mem::size_of::<D3D10_VIEWPORT>() == 24);
const _: () = assert!(core::mem::align_of::<D3D10_VIEWPORT>() == 4);
impl Default for D3D10_VIEWPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, TopLeftX) == 0);
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, TopLeftY) == 4);
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, Width) == 8);
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, Height) == 12);
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, MinDepth) == 16);
const _: () = assert!(core::mem::offset_of!(D3D10_VIEWPORT, MaxDepth) == 20);
impl crate::GuestArg for D3D10_VIEWPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BLEND_DESC {
    pub AlphaToCoverageEnable: i32,
    pub IndependentBlendEnable: i32,
    pub RenderTarget: [D3D11_RENDER_TARGET_BLEND_DESC; 8],
}
const _: () = assert!(core::mem::size_of::<D3D11_BLEND_DESC>() == 264);
const _: () = assert!(core::mem::align_of::<D3D11_BLEND_DESC>() == 4);
impl Default for D3D11_BLEND_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC, AlphaToCoverageEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC, IndependentBlendEnable) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC, RenderTarget) == 8);
impl crate::GuestArg for D3D11_BLEND_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 66;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BLEND_DESC1 {
    pub AlphaToCoverageEnable: i32,
    pub IndependentBlendEnable: i32,
    pub RenderTarget: [D3D11_RENDER_TARGET_BLEND_DESC1; 8],
}
const _: () = assert!(core::mem::size_of::<D3D11_BLEND_DESC1>() == 328);
const _: () = assert!(core::mem::align_of::<D3D11_BLEND_DESC1>() == 4);
impl Default for D3D11_BLEND_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC1, AlphaToCoverageEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC1, IndependentBlendEnable) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BLEND_DESC1, RenderTarget) == 8);
impl crate::GuestArg for D3D11_BLEND_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 82;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BOX {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BOX>() == 24);
const _: () = assert!(core::mem::align_of::<D3D11_BOX>() == 4);
impl Default for D3D11_BOX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, left) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, top) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, front) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, right) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, bottom) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_BOX, back) == 20);
impl crate::GuestArg for D3D11_BOX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BUFFEREX_SRV {
    pub FirstElement: u32,
    pub NumElements: u32,
    pub Flags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BUFFEREX_SRV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_BUFFEREX_SRV>() == 4);
impl Default for D3D11_BUFFEREX_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFEREX_SRV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFEREX_SRV, NumElements) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFEREX_SRV, Flags) == 8);
impl crate::GuestArg for D3D11_BUFFEREX_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BUFFER_DESC {
    pub ByteWidth: u32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
    pub StructureByteStride: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BUFFER_DESC>() == 24);
const _: () = assert!(core::mem::align_of::<D3D11_BUFFER_DESC>() == 4);
impl Default for D3D11_BUFFER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, ByteWidth) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, Usage) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, BindFlags) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, CPUAccessFlags) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, MiscFlags) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_DESC, StructureByteStride) == 20);
impl crate::GuestArg for D3D11_BUFFER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BUFFER_RTV {
    pub FirstElement: u32,
    pub NumElements: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BUFFER_RTV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_BUFFER_RTV>() == 4);
impl Default for D3D11_BUFFER_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_RTV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_RTV, NumElements) == 4);
impl crate::GuestArg for D3D11_BUFFER_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BUFFER_SRV {
    pub FirstElement: u32,
    pub NumElements: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BUFFER_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_BUFFER_SRV>() == 4);
impl Default for D3D11_BUFFER_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_SRV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_SRV, NumElements) == 4);
impl crate::GuestArg for D3D11_BUFFER_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_BUFFER_UAV {
    pub FirstElement: u32,
    pub NumElements: u32,
    pub Flags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_BUFFER_UAV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_BUFFER_UAV>() == 4);
impl Default for D3D11_BUFFER_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_UAV, FirstElement) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_UAV, NumElements) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_BUFFER_UAV, Flags) == 8);
impl crate::GuestArg for D3D11_BUFFER_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_CLASS_INSTANCE_DESC {
    pub InstanceId: u32,
    pub InstanceIndex: u32,
    pub TypeId: u32,
    pub ConstantBuffer: u32,
    pub BaseConstantBufferOffset: u32,
    pub BaseTexture: u32,
    pub BaseSampler: u32,
    pub Created: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_CLASS_INSTANCE_DESC>() == 32);
const _: () = assert!(core::mem::align_of::<D3D11_CLASS_INSTANCE_DESC>() == 4);
impl Default for D3D11_CLASS_INSTANCE_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, InstanceId) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, InstanceIndex) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, TypeId) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, ConstantBuffer) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, BaseConstantBufferOffset) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, BaseTexture) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, BaseSampler) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_CLASS_INSTANCE_DESC, Created) == 28);
impl crate::GuestArg for D3D11_CLASS_INSTANCE_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_COUNTER_DESC {
    pub Counter: i32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_COUNTER_DESC>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_COUNTER_DESC>() == 4);
impl Default for D3D11_COUNTER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_COUNTER_DESC, Counter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_COUNTER_DESC, MiscFlags) == 4);
impl crate::GuestArg for D3D11_COUNTER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_COUNTER_INFO {
    pub LastDeviceDependentCounter: i32,
    pub NumSimultaneousCounters: u32,
    pub NumDetectableParallelUnits: u8,
    pub _padding0: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<D3D11_COUNTER_INFO>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_COUNTER_INFO>() == 4);
impl Default for D3D11_COUNTER_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_COUNTER_INFO, LastDeviceDependentCounter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_COUNTER_INFO, NumSimultaneousCounters) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_COUNTER_INFO, NumDetectableParallelUnits) == 8);
impl crate::GuestArg for D3D11_COUNTER_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_DEPTH_STENCILOP_DESC {
    pub StencilFailOp: i32,
    pub StencilDepthFailOp: i32,
    pub StencilPassOp: i32,
    pub StencilFunc: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_DEPTH_STENCILOP_DESC>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_DEPTH_STENCILOP_DESC>() == 4);
impl Default for D3D11_DEPTH_STENCILOP_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCILOP_DESC, StencilFailOp) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCILOP_DESC, StencilDepthFailOp) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCILOP_DESC, StencilPassOp) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCILOP_DESC, StencilFunc) == 12);
impl crate::GuestArg for D3D11_DEPTH_STENCILOP_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_DEPTH_STENCIL_DESC {
    pub DepthEnable: i32,
    pub DepthWriteMask: i32,
    pub DepthFunc: i32,
    pub StencilEnable: i32,
    pub StencilReadMask: u8,
    pub StencilWriteMask: u8,
    pub _padding0: [u8; 2],
    pub FrontFace: D3D11_DEPTH_STENCILOP_DESC,
    pub BackFace: D3D11_DEPTH_STENCILOP_DESC,
}
const _: () = assert!(core::mem::size_of::<D3D11_DEPTH_STENCIL_DESC>() == 52);
const _: () = assert!(core::mem::align_of::<D3D11_DEPTH_STENCIL_DESC>() == 4);
impl Default for D3D11_DEPTH_STENCIL_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, DepthEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, DepthWriteMask) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, DepthFunc) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, StencilEnable) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, StencilReadMask) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, StencilWriteMask) == 17);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, FrontFace) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_DESC, BackFace) == 36);
impl crate::GuestArg for D3D11_DEPTH_STENCIL_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_DEPTH_STENCIL_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Flags: u32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<D3D11_DEPTH_STENCIL_VIEW_DESC>() == 24);
const _: () = assert!(core::mem::align_of::<D3D11_DEPTH_STENCIL_VIEW_DESC>() == 4);
impl Default for D3D11_DEPTH_STENCIL_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_VIEW_DESC, Flags) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_DEPTH_STENCIL_VIEW_DESC, Anonymous) == 12);
impl crate::GuestArg for D3D11_DEPTH_STENCIL_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS {
    pub IndexCountPerInstance: u32,
    pub InstanceCount: u32,
    pub StartIndexLocation: u32,
    pub BaseVertexLocation: i32,
    pub StartInstanceLocation: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS>() == 20);
const _: () = assert!(core::mem::align_of::<D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS>() == 4);
impl Default for D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS, IndexCountPerInstance) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS, InstanceCount) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS, StartIndexLocation) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS, BaseVertexLocation) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS, StartInstanceLocation) == 16);
impl crate::GuestArg for D3D11_DRAW_INDEXED_INSTANCED_INDIRECT_ARGS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_DRAW_INSTANCED_INDIRECT_ARGS {
    pub VertexCountPerInstance: u32,
    pub InstanceCount: u32,
    pub StartVertexLocation: u32,
    pub StartInstanceLocation: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_DRAW_INSTANCED_INDIRECT_ARGS>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_DRAW_INSTANCED_INDIRECT_ARGS>() == 4);
impl Default for D3D11_DRAW_INSTANCED_INDIRECT_ARGS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INSTANCED_INDIRECT_ARGS, VertexCountPerInstance) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INSTANCED_INDIRECT_ARGS, InstanceCount) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INSTANCED_INDIRECT_ARGS, StartVertexLocation) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_DRAW_INSTANCED_INDIRECT_ARGS, StartInstanceLocation) == 12);
impl crate::GuestArg for D3D11_DRAW_INSTANCED_INDIRECT_ARGS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_ARCHITECTURE_INFO {
    pub TileBasedDeferredRenderer: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_ARCHITECTURE_INFO>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_ARCHITECTURE_INFO>() == 4);
impl Default for D3D11_FEATURE_DATA_ARCHITECTURE_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_ARCHITECTURE_INFO, TileBasedDeferredRenderer) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_ARCHITECTURE_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS {
    pub ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS, ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS {
    pub OutputMergerLogicOp: i32,
    pub UAVOnlyRenderingForcedSampleCount: i32,
    pub DiscardAPIsSeenByDriver: i32,
    pub FlagsForUpdateAndCopySeenByDriver: i32,
    pub ClearView: i32,
    pub CopyWithOverlap: i32,
    pub ConstantBufferPartialUpdate: i32,
    pub ConstantBufferOffsetting: i32,
    pub MapNoOverwriteOnDynamicConstantBuffer: i32,
    pub MapNoOverwriteOnDynamicBufferSRV: i32,
    pub MultisampleRTVWithForcedSampleCountOne: i32,
    pub SAD4ShaderInstructions: i32,
    pub ExtendedDoublesShaderInstructions: i32,
    pub ExtendedResourceSharing: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS>() == 56);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, OutputMergerLogicOp) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, UAVOnlyRenderingForcedSampleCount) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, DiscardAPIsSeenByDriver) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, FlagsForUpdateAndCopySeenByDriver) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, ClearView) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, CopyWithOverlap) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, ConstantBufferPartialUpdate) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, ConstantBufferOffsetting) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, MapNoOverwriteOnDynamicConstantBuffer) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, MapNoOverwriteOnDynamicBufferSRV) == 36);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, MultisampleRTVWithForcedSampleCountOne) == 40);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, SAD4ShaderInstructions) == 44);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, ExtendedDoublesShaderInstructions) == 48);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS, ExtendedResourceSharing) == 52);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 14;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS1 {
    pub TiledResourcesTier: i32,
    pub MinMaxFiltering: i32,
    pub ClearViewAlsoSupportsDepthOnlyFormats: i32,
    pub MapOnDefaultBuffers: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS1>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS1>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS1, TiledResourcesTier) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS1, MinMaxFiltering) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS1, ClearViewAlsoSupportsDepthOnlyFormats) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS1, MapOnDefaultBuffers) == 12);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS2 {
    pub PSSpecifiedStencilRefSupported: i32,
    pub TypedUAVLoadAdditionalFormats: i32,
    pub ROVsSupported: i32,
    pub ConservativeRasterizationTier: i32,
    pub TiledResourcesTier: i32,
    pub MapOnDefaultTextures: i32,
    pub StandardSwizzle: i32,
    pub UnifiedMemoryArchitecture: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS2>() == 32);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS2>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, PSSpecifiedStencilRefSupported) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, TypedUAVLoadAdditionalFormats) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, ROVsSupported) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, ConservativeRasterizationTier) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, TiledResourcesTier) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, MapOnDefaultTextures) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, StandardSwizzle) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS2, UnifiedMemoryArchitecture) == 28);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS2 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS3 {
    pub VPAndRTArrayIndexFromAnyShaderFeedingRasterizer: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS3>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS3>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS3 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS3, VPAndRTArrayIndexFromAnyShaderFeedingRasterizer) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS3 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS4 {
    pub ExtendedNV12SharedTextureSupported: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS4>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS4>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS4 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS4, ExtendedNV12SharedTextureSupported) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS4 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D11_OPTIONS5 {
    pub SharedResourceTier: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS5>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS5>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D11_OPTIONS5 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D11_OPTIONS5, SharedResourceTier) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D11_OPTIONS5 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D9_OPTIONS {
    pub FullNonPow2TextureSupport: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D9_OPTIONS>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D9_OPTIONS>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D9_OPTIONS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_OPTIONS, FullNonPow2TextureSupport) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D9_OPTIONS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D9_OPTIONS1 {
    pub FullNonPow2TextureSupported: i32,
    pub DepthAsTextureWithLessEqualComparisonFilterSupported: i32,
    pub SimpleInstancingSupported: i32,
    pub TextureCubeFaceRenderTargetWithNonCubeDepthStencilSupported: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D9_OPTIONS1>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D9_OPTIONS1>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D9_OPTIONS1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_OPTIONS1, FullNonPow2TextureSupported) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_OPTIONS1, DepthAsTextureWithLessEqualComparisonFilterSupported) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_OPTIONS1, SimpleInstancingSupported) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_OPTIONS1, TextureCubeFaceRenderTargetWithNonCubeDepthStencilSupported) == 12);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D9_OPTIONS1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT {
    pub SupportsDepthAsTextureWithLessEqualComparisonFilter: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT, SupportsDepthAsTextureWithLessEqualComparisonFilter) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT {
    pub SimpleInstancingSupported: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT, SimpleInstancingSupported) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_DISPLAYABLE {
    pub DisplayableTexture: i32,
    pub SharedResourceTier: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_DISPLAYABLE>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_DISPLAYABLE>() == 4);
impl Default for D3D11_FEATURE_DATA_DISPLAYABLE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_DISPLAYABLE, DisplayableTexture) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_DISPLAYABLE, SharedResourceTier) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_DISPLAYABLE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_DOUBLES {
    pub DoublePrecisionFloatShaderOps: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_DOUBLES>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_DOUBLES>() == 4);
impl Default for D3D11_FEATURE_DATA_DOUBLES { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_DOUBLES, DoublePrecisionFloatShaderOps) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_DOUBLES {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_FORMAT_SUPPORT {
    pub InFormat: i32,
    pub OutFormatSupport: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_FORMAT_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_FORMAT_SUPPORT, InFormat) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_FORMAT_SUPPORT, OutFormatSupport) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_FORMAT_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_FORMAT_SUPPORT2 {
    pub InFormat: i32,
    pub OutFormatSupport2: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT2>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT2>() == 4);
impl Default for D3D11_FEATURE_DATA_FORMAT_SUPPORT2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_FORMAT_SUPPORT2, InFormat) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_FORMAT_SUPPORT2, OutFormatSupport2) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_FORMAT_SUPPORT2 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT {
    pub MaxGPUVirtualAddressBitsPerResource: u32,
    pub MaxGPUVirtualAddressBitsPerProcess: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT, MaxGPUVirtualAddressBitsPerResource) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT, MaxGPUVirtualAddressBitsPerProcess) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_MARKER_SUPPORT {
    pub Profile: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_MARKER_SUPPORT>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_MARKER_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_MARKER_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_MARKER_SUPPORT, Profile) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_MARKER_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_SHADER_CACHE {
    pub SupportFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_SHADER_CACHE>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_SHADER_CACHE>() == 4);
impl Default for D3D11_FEATURE_DATA_SHADER_CACHE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_SHADER_CACHE, SupportFlags) == 0);
impl crate::GuestArg for D3D11_FEATURE_DATA_SHADER_CACHE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT {
    pub PixelShaderMinPrecision: u32,
    pub AllOtherShaderStagesMinPrecision: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT>() == 4);
impl Default for D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT, PixelShaderMinPrecision) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT, AllOtherShaderStagesMinPrecision) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_FEATURE_DATA_THREADING {
    pub DriverConcurrentCreates: i32,
    pub DriverCommandLists: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_FEATURE_DATA_THREADING>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_FEATURE_DATA_THREADING>() == 4);
impl Default for D3D11_FEATURE_DATA_THREADING { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_THREADING, DriverConcurrentCreates) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_FEATURE_DATA_THREADING, DriverCommandLists) == 4);
impl crate::GuestArg for D3D11_FEATURE_DATA_THREADING {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_INPUT_ELEMENT_DESC {
    pub SemanticName: u32,
    pub SemanticIndex: u32,
    pub Format: i32,
    pub InputSlot: u32,
    pub AlignedByteOffset: u32,
    pub InputSlotClass: i32,
    pub InstanceDataStepRate: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_INPUT_ELEMENT_DESC>() == 28);
const _: () = assert!(core::mem::align_of::<D3D11_INPUT_ELEMENT_DESC>() == 4);
impl Default for D3D11_INPUT_ELEMENT_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, SemanticName) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, SemanticIndex) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, Format) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, InputSlot) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, AlignedByteOffset) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, InputSlotClass) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_INPUT_ELEMENT_DESC, InstanceDataStepRate) == 24);
impl crate::GuestArg for D3D11_INPUT_ELEMENT_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 7;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_MAPPED_SUBRESOURCE {
    pub pData: u32,
    pub RowPitch: u32,
    pub DepthPitch: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_MAPPED_SUBRESOURCE>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_MAPPED_SUBRESOURCE>() == 4);
impl Default for D3D11_MAPPED_SUBRESOURCE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_MAPPED_SUBRESOURCE, pData) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_MAPPED_SUBRESOURCE, RowPitch) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_MAPPED_SUBRESOURCE, DepthPitch) == 8);
impl crate::GuestArg for D3D11_MAPPED_SUBRESOURCE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_QUERY_DATA_PIPELINE_STATISTICS {
    pub IAVertices: u64,
    pub IAPrimitives: u64,
    pub VSInvocations: u64,
    pub GSInvocations: u64,
    pub GSPrimitives: u64,
    pub CInvocations: u64,
    pub CPrimitives: u64,
    pub PSInvocations: u64,
    pub HSInvocations: u64,
    pub DSInvocations: u64,
    pub CSInvocations: u64,
}
const _: () = assert!(core::mem::size_of::<D3D11_QUERY_DATA_PIPELINE_STATISTICS>() == 88);
const _: () = assert!(core::mem::align_of::<D3D11_QUERY_DATA_PIPELINE_STATISTICS>() == 8);
impl Default for D3D11_QUERY_DATA_PIPELINE_STATISTICS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, IAVertices) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, IAPrimitives) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, VSInvocations) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, GSInvocations) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, GSPrimitives) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, CInvocations) == 40);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, CPrimitives) == 48);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, PSInvocations) == 56);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, HSInvocations) == 64);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, DSInvocations) == 72);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_PIPELINE_STATISTICS, CSInvocations) == 80);
impl crate::GuestArg for D3D11_QUERY_DATA_PIPELINE_STATISTICS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 22;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_QUERY_DATA_SO_STATISTICS {
    pub NumPrimitivesWritten: u64,
    pub PrimitivesStorageNeeded: u64,
}
const _: () = assert!(core::mem::size_of::<D3D11_QUERY_DATA_SO_STATISTICS>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_QUERY_DATA_SO_STATISTICS>() == 8);
impl Default for D3D11_QUERY_DATA_SO_STATISTICS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_SO_STATISTICS, NumPrimitivesWritten) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_SO_STATISTICS, PrimitivesStorageNeeded) == 8);
impl crate::GuestArg for D3D11_QUERY_DATA_SO_STATISTICS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_QUERY_DATA_TIMESTAMP_DISJOINT {
    pub Frequency: u64,
    pub Disjoint: i32,
    pub _padding0: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() == 8);
impl Default for D3D11_QUERY_DATA_TIMESTAMP_DISJOINT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, Frequency) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, Disjoint) == 8);
impl crate::GuestArg for D3D11_QUERY_DATA_TIMESTAMP_DISJOINT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_QUERY_DESC {
    pub Query: i32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_QUERY_DESC>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_QUERY_DESC>() == 4);
impl Default for D3D11_QUERY_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DESC, Query) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_QUERY_DESC, MiscFlags) == 4);
impl crate::GuestArg for D3D11_QUERY_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_RASTERIZER_DESC {
    pub FillMode: i32,
    pub CullMode: i32,
    pub FrontCounterClockwise: i32,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: i32,
    pub ScissorEnable: i32,
    pub MultisampleEnable: i32,
    pub AntialiasedLineEnable: i32,
}
const _: () = assert!(core::mem::size_of::<D3D11_RASTERIZER_DESC>() == 40);
const _: () = assert!(core::mem::align_of::<D3D11_RASTERIZER_DESC>() == 4);
impl Default for D3D11_RASTERIZER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, FillMode) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, CullMode) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, FrontCounterClockwise) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, DepthBias) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, DepthBiasClamp) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, SlopeScaledDepthBias) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, DepthClipEnable) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, ScissorEnable) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, MultisampleEnable) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC, AntialiasedLineEnable) == 36);
impl crate::GuestArg for D3D11_RASTERIZER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 10;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_RASTERIZER_DESC1 {
    pub FillMode: i32,
    pub CullMode: i32,
    pub FrontCounterClockwise: i32,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: i32,
    pub ScissorEnable: i32,
    pub MultisampleEnable: i32,
    pub AntialiasedLineEnable: i32,
    pub ForcedSampleCount: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_RASTERIZER_DESC1>() == 44);
const _: () = assert!(core::mem::align_of::<D3D11_RASTERIZER_DESC1>() == 4);
impl Default for D3D11_RASTERIZER_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, FillMode) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, CullMode) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, FrontCounterClockwise) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, DepthBias) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, DepthBiasClamp) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, SlopeScaledDepthBias) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, DepthClipEnable) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, ScissorEnable) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, MultisampleEnable) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, AntialiasedLineEnable) == 36);
const _: () = assert!(core::mem::offset_of!(D3D11_RASTERIZER_DESC1, ForcedSampleCount) == 40);
impl crate::GuestArg for D3D11_RASTERIZER_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_RENDER_TARGET_BLEND_DESC {
    pub BlendEnable: i32,
    pub SrcBlend: i32,
    pub DestBlend: i32,
    pub BlendOp: i32,
    pub SrcBlendAlpha: i32,
    pub DestBlendAlpha: i32,
    pub BlendOpAlpha: i32,
    pub RenderTargetWriteMask: u8,
    pub _padding0: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<D3D11_RENDER_TARGET_BLEND_DESC>() == 32);
const _: () = assert!(core::mem::align_of::<D3D11_RENDER_TARGET_BLEND_DESC>() == 4);
impl Default for D3D11_RENDER_TARGET_BLEND_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, BlendEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, SrcBlend) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, DestBlend) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, BlendOp) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, SrcBlendAlpha) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, DestBlendAlpha) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, BlendOpAlpha) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC, RenderTargetWriteMask) == 28);
impl crate::GuestArg for D3D11_RENDER_TARGET_BLEND_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_RENDER_TARGET_BLEND_DESC1 {
    pub BlendEnable: i32,
    pub LogicOpEnable: i32,
    pub SrcBlend: i32,
    pub DestBlend: i32,
    pub BlendOp: i32,
    pub SrcBlendAlpha: i32,
    pub DestBlendAlpha: i32,
    pub BlendOpAlpha: i32,
    pub LogicOp: i32,
    pub RenderTargetWriteMask: u8,
    pub _padding0: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<D3D11_RENDER_TARGET_BLEND_DESC1>() == 40);
const _: () = assert!(core::mem::align_of::<D3D11_RENDER_TARGET_BLEND_DESC1>() == 4);
impl Default for D3D11_RENDER_TARGET_BLEND_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, BlendEnable) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, LogicOpEnable) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, SrcBlend) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, DestBlend) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, BlendOp) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, SrcBlendAlpha) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, DestBlendAlpha) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, BlendOpAlpha) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, LogicOp) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_BLEND_DESC1, RenderTargetWriteMask) == 36);
impl crate::GuestArg for D3D11_RENDER_TARGET_BLEND_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 10;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_RENDER_TARGET_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<D3D11_RENDER_TARGET_VIEW_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3D11_RENDER_TARGET_VIEW_DESC>() == 4);
impl Default for D3D11_RENDER_TARGET_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_RENDER_TARGET_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D11_RENDER_TARGET_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_SAMPLER_DESC {
    pub Filter: i32,
    pub AddressU: i32,
    pub AddressV: i32,
    pub AddressW: i32,
    pub MipLODBias: f32,
    pub MaxAnisotropy: u32,
    pub ComparisonFunc: i32,
    pub BorderColor: [f32; 4],
    pub MinLOD: f32,
    pub MaxLOD: f32,
}
const _: () = assert!(core::mem::size_of::<D3D11_SAMPLER_DESC>() == 52);
const _: () = assert!(core::mem::align_of::<D3D11_SAMPLER_DESC>() == 4);
impl Default for D3D11_SAMPLER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, Filter) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, AddressU) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, AddressV) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, AddressW) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, MipLODBias) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, MaxAnisotropy) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, ComparisonFunc) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, BorderColor) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, MinLOD) == 44);
const _: () = assert!(core::mem::offset_of!(D3D11_SAMPLER_DESC, MaxLOD) == 48);
impl crate::GuestArg for D3D11_SAMPLER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_SHADER_RESOURCE_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 16],
}
const _: () = assert!(core::mem::size_of::<D3D11_SHADER_RESOURCE_VIEW_DESC>() == 24);
const _: () = assert!(core::mem::align_of::<D3D11_SHADER_RESOURCE_VIEW_DESC>() == 4);
impl Default for D3D11_SHADER_RESOURCE_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_SHADER_RESOURCE_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_SHADER_RESOURCE_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_SHADER_RESOURCE_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D11_SHADER_RESOURCE_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_SO_DECLARATION_ENTRY {
    pub Stream: u32,
    pub SemanticName: u32,
    pub SemanticIndex: u32,
    pub StartComponent: u8,
    pub ComponentCount: u8,
    pub OutputSlot: u8,
    pub _padding0: [u8; 1],
}
const _: () = assert!(core::mem::size_of::<D3D11_SO_DECLARATION_ENTRY>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_SO_DECLARATION_ENTRY>() == 4);
impl Default for D3D11_SO_DECLARATION_ENTRY { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, Stream) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, SemanticName) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, SemanticIndex) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, StartComponent) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, ComponentCount) == 13);
const _: () = assert!(core::mem::offset_of!(D3D11_SO_DECLARATION_ENTRY, OutputSlot) == 14);
impl crate::GuestArg for D3D11_SO_DECLARATION_ENTRY {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_SUBRESOURCE_DATA {
    pub pSysMem: u32,
    pub SysMemPitch: u32,
    pub SysMemSlicePitch: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_SUBRESOURCE_DATA>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_SUBRESOURCE_DATA>() == 4);
impl Default for D3D11_SUBRESOURCE_DATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_SUBRESOURCE_DATA, pSysMem) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_SUBRESOURCE_DATA, SysMemPitch) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_SUBRESOURCE_DATA, SysMemSlicePitch) == 8);
impl crate::GuestArg for D3D11_SUBRESOURCE_DATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_ARRAY_DSV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_ARRAY_DSV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_ARRAY_DSV>() == 4);
impl Default for D3D11_TEX1D_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_DSV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_DSV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_DSV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX1D_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_ARRAY_RTV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_ARRAY_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_ARRAY_RTV>() == 4);
impl Default for D3D11_TEX1D_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_RTV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_RTV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX1D_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_ARRAY_SRV>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_ARRAY_SRV>() == 4);
impl Default for D3D11_TEX1D_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_SRV, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_SRV, FirstArraySlice) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_SRV, ArraySize) == 12);
impl crate::GuestArg for D3D11_TEX1D_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_ARRAY_UAV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_ARRAY_UAV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_ARRAY_UAV>() == 4);
impl Default for D3D11_TEX1D_ARRAY_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_UAV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_UAV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_ARRAY_UAV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX1D_ARRAY_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_DSV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_DSV>() == 4);
impl Default for D3D11_TEX1D_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_DSV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX1D_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_RTV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_RTV>() == 4);
impl Default for D3D11_TEX1D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_RTV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX1D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_SRV>() == 4);
impl Default for D3D11_TEX1D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D11_TEX1D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX1D_UAV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX1D_UAV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX1D_UAV>() == 4);
impl Default for D3D11_TEX1D_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX1D_UAV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX1D_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_ARRAY_DSV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_ARRAY_DSV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_ARRAY_DSV>() == 4);
impl Default for D3D11_TEX2DMS_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_DSV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_DSV, ArraySize) == 4);
impl crate::GuestArg for D3D11_TEX2DMS_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_ARRAY_RTV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_ARRAY_RTV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_ARRAY_RTV>() == 4);
impl Default for D3D11_TEX2DMS_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_RTV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_RTV, ArraySize) == 4);
impl crate::GuestArg for D3D11_TEX2DMS_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_ARRAY_SRV {
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_ARRAY_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_ARRAY_SRV>() == 4);
impl Default for D3D11_TEX2DMS_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_SRV, FirstArraySlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_ARRAY_SRV, ArraySize) == 4);
impl crate::GuestArg for D3D11_TEX2DMS_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_DSV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_DSV>() == 4);
impl Default for D3D11_TEX2DMS_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_DSV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D11_TEX2DMS_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_RTV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_RTV>() == 4);
impl Default for D3D11_TEX2DMS_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_RTV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D11_TEX2DMS_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2DMS_SRV {
    pub UnusedField_NothingToDefine: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2DMS_SRV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2DMS_SRV>() == 4);
impl Default for D3D11_TEX2DMS_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2DMS_SRV, UnusedField_NothingToDefine) == 0);
impl crate::GuestArg for D3D11_TEX2DMS_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_ARRAY_DSV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_ARRAY_DSV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_ARRAY_DSV>() == 4);
impl Default for D3D11_TEX2D_ARRAY_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_DSV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_DSV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_DSV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX2D_ARRAY_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_ARRAY_RTV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_ARRAY_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_ARRAY_RTV>() == 4);
impl Default for D3D11_TEX2D_ARRAY_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_RTV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_RTV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX2D_ARRAY_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_ARRAY_SRV>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_ARRAY_SRV>() == 4);
impl Default for D3D11_TEX2D_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_SRV, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_SRV, FirstArraySlice) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_SRV, ArraySize) == 12);
impl crate::GuestArg for D3D11_TEX2D_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_ARRAY_UAV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_ARRAY_UAV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_ARRAY_UAV>() == 4);
impl Default for D3D11_TEX2D_ARRAY_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_UAV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_UAV, FirstArraySlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_ARRAY_UAV, ArraySize) == 8);
impl crate::GuestArg for D3D11_TEX2D_ARRAY_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_DSV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_DSV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_DSV>() == 4);
impl Default for D3D11_TEX2D_DSV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_DSV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX2D_DSV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_RTV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_RTV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_RTV>() == 4);
impl Default for D3D11_TEX2D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_RTV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX2D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_SRV>() == 4);
impl Default for D3D11_TEX2D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D11_TEX2D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX2D_UAV {
    pub MipSlice: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX2D_UAV>() == 4);
const _: () = assert!(core::mem::align_of::<D3D11_TEX2D_UAV>() == 4);
impl Default for D3D11_TEX2D_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX2D_UAV, MipSlice) == 0);
impl crate::GuestArg for D3D11_TEX2D_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX3D_RTV {
    pub MipSlice: u32,
    pub FirstWSlice: u32,
    pub WSize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX3D_RTV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX3D_RTV>() == 4);
impl Default for D3D11_TEX3D_RTV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_RTV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_RTV, FirstWSlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_RTV, WSize) == 8);
impl crate::GuestArg for D3D11_TEX3D_RTV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX3D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX3D_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEX3D_SRV>() == 4);
impl Default for D3D11_TEX3D_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D11_TEX3D_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEX3D_UAV {
    pub MipSlice: u32,
    pub FirstWSlice: u32,
    pub WSize: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEX3D_UAV>() == 12);
const _: () = assert!(core::mem::align_of::<D3D11_TEX3D_UAV>() == 4);
impl Default for D3D11_TEX3D_UAV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_UAV, MipSlice) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_UAV, FirstWSlice) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEX3D_UAV, WSize) == 8);
impl crate::GuestArg for D3D11_TEX3D_UAV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEXCUBE_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub First2DArrayFace: u32,
    pub NumCubes: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEXCUBE_ARRAY_SRV>() == 16);
const _: () = assert!(core::mem::align_of::<D3D11_TEXCUBE_ARRAY_SRV>() == 4);
impl Default for D3D11_TEXCUBE_ARRAY_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_ARRAY_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_ARRAY_SRV, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_ARRAY_SRV, First2DArrayFace) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_ARRAY_SRV, NumCubes) == 12);
impl crate::GuestArg for D3D11_TEXCUBE_ARRAY_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEXCUBE_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEXCUBE_SRV>() == 8);
const _: () = assert!(core::mem::align_of::<D3D11_TEXCUBE_SRV>() == 4);
impl Default for D3D11_TEXCUBE_SRV { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_SRV, MostDetailedMip) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXCUBE_SRV, MipLevels) == 4);
impl crate::GuestArg for D3D11_TEXCUBE_SRV {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEXTURE1D_DESC {
    pub Width: u32,
    pub MipLevels: u32,
    pub ArraySize: u32,
    pub Format: i32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEXTURE1D_DESC>() == 32);
const _: () = assert!(core::mem::align_of::<D3D11_TEXTURE1D_DESC>() == 4);
impl Default for D3D11_TEXTURE1D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, MipLevels) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, ArraySize) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, Format) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, Usage) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, BindFlags) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, CPUAccessFlags) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE1D_DESC, MiscFlags) == 28);
impl crate::GuestArg for D3D11_TEXTURE1D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEXTURE2D_DESC {
    pub Width: u32,
    pub Height: u32,
    pub MipLevels: u32,
    pub ArraySize: u32,
    pub Format: i32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEXTURE2D_DESC>() == 44);
const _: () = assert!(core::mem::align_of::<D3D11_TEXTURE2D_DESC>() == 4);
impl Default for D3D11_TEXTURE2D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, MipLevels) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, ArraySize) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, Format) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, SampleDesc) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, Usage) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, BindFlags) == 32);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, CPUAccessFlags) == 36);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE2D_DESC, MiscFlags) == 40);
impl crate::GuestArg for D3D11_TEXTURE2D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_TEXTURE3D_DESC {
    pub Width: u32,
    pub Height: u32,
    pub Depth: u32,
    pub MipLevels: u32,
    pub Format: i32,
    pub Usage: i32,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3D11_TEXTURE3D_DESC>() == 36);
const _: () = assert!(core::mem::align_of::<D3D11_TEXTURE3D_DESC>() == 4);
impl Default for D3D11_TEXTURE3D_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, Depth) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, MipLevels) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, Format) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, Usage) == 20);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, BindFlags) == 24);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, CPUAccessFlags) == 28);
const _: () = assert!(core::mem::offset_of!(D3D11_TEXTURE3D_DESC, MiscFlags) == 32);
impl crate::GuestArg for D3D11_TEXTURE3D_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 9;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_UNORDERED_ACCESS_VIEW_DESC {
    pub Format: i32,
    pub ViewDimension: i32,
    pub Anonymous: [u8; 12],
}
const _: () = assert!(core::mem::size_of::<D3D11_UNORDERED_ACCESS_VIEW_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3D11_UNORDERED_ACCESS_VIEW_DESC>() == 4);
impl Default for D3D11_UNORDERED_ACCESS_VIEW_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_UNORDERED_ACCESS_VIEW_DESC, Format) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_UNORDERED_ACCESS_VIEW_DESC, ViewDimension) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_UNORDERED_ACCESS_VIEW_DESC, Anonymous) == 8);
impl crate::GuestArg for D3D11_UNORDERED_ACCESS_VIEW_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3D11_VIEWPORT {
    pub TopLeftX: f32,
    pub TopLeftY: f32,
    pub Width: f32,
    pub Height: f32,
    pub MinDepth: f32,
    pub MaxDepth: f32,
}
const _: () = assert!(core::mem::size_of::<D3D11_VIEWPORT>() == 24);
const _: () = assert!(core::mem::align_of::<D3D11_VIEWPORT>() == 4);
impl Default for D3D11_VIEWPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, TopLeftX) == 0);
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, TopLeftY) == 4);
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, Width) == 8);
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, Height) == 12);
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, MinDepth) == 16);
const _: () = assert!(core::mem::offset_of!(D3D11_VIEWPORT, MaxDepth) == 20);
impl crate::GuestArg for D3D11_VIEWPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DBRANCH {
    pub dwMask: u32,
    pub dwValue: u32,
    pub bNegate: i32,
    pub dwOffset: u32,
}
const _: () = assert!(core::mem::size_of::<D3DBRANCH>() == 16);
const _: () = assert!(core::mem::align_of::<D3DBRANCH>() == 4);
impl Default for D3DBRANCH { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DBRANCH, dwMask) == 0);
const _: () = assert!(core::mem::offset_of!(D3DBRANCH, dwValue) == 4);
const _: () = assert!(core::mem::offset_of!(D3DBRANCH, bNegate) == 8);
const _: () = assert!(core::mem::offset_of!(D3DBRANCH, dwOffset) == 12);
impl crate::GuestArg for D3DBRANCH {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DCLIPSTATUS {
    pub dwFlags: u32,
    pub dwStatus: u32,
    pub minx: f32,
    pub maxx: f32,
    pub miny: f32,
    pub maxy: f32,
    pub minz: f32,
    pub maxz: f32,
}
const _: () = assert!(core::mem::size_of::<D3DCLIPSTATUS>() == 32);
const _: () = assert!(core::mem::align_of::<D3DCLIPSTATUS>() == 4);
impl Default for D3DCLIPSTATUS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, dwFlags) == 0);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, dwStatus) == 4);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, minx) == 8);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, maxx) == 12);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, miny) == 16);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, maxy) == 20);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, minz) == 24);
const _: () = assert!(core::mem::offset_of!(D3DCLIPSTATUS, maxz) == 28);
impl crate::GuestArg for D3DCLIPSTATUS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DCOLORVALUE {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}
const _: () = assert!(core::mem::size_of::<D3DCOLORVALUE>() == 16);
const _: () = assert!(core::mem::align_of::<D3DCOLORVALUE>() == 4);
impl Default for D3DCOLORVALUE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DCOLORVALUE, r) == 0);
const _: () = assert!(core::mem::offset_of!(D3DCOLORVALUE, g) == 4);
const _: () = assert!(core::mem::offset_of!(D3DCOLORVALUE, b) == 8);
const _: () = assert!(core::mem::offset_of!(D3DCOLORVALUE, a) == 12);
impl crate::GuestArg for D3DCOLORVALUE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDEVICEDESC {
    pub dwSize: u32,
    pub dwFlags: u32,
    pub dcmColorModel: u32,
    pub dwDevCaps: u32,
    pub dtcTransformCaps: D3DTRANSFORMCAPS,
    pub bClipping: i32,
    pub dlcLightingCaps: D3DLIGHTINGCAPS,
    pub dpcLineCaps: D3DPRIMCAPS,
    pub dpcTriCaps: D3DPRIMCAPS,
    pub dwDeviceRenderBitDepth: u32,
    pub dwDeviceZBufferBitDepth: u32,
    pub dwMaxBufferSize: u32,
    pub dwMaxVertexCount: u32,
    pub dwMinTextureWidth: u32,
    pub dwMinTextureHeight: u32,
    pub dwMaxTextureWidth: u32,
    pub dwMaxTextureHeight: u32,
    pub dwMinStippleWidth: u32,
    pub dwMaxStippleWidth: u32,
    pub dwMinStippleHeight: u32,
    pub dwMaxStippleHeight: u32,
    pub dwMaxTextureRepeat: u32,
    pub dwMaxTextureAspectRatio: u32,
    pub dwMaxAnisotropy: u32,
    pub dvGuardBandLeft: f32,
    pub dvGuardBandTop: f32,
    pub dvGuardBandRight: f32,
    pub dvGuardBandBottom: f32,
    pub dvExtentsAdjust: f32,
    pub dwStencilCaps: u32,
    pub dwFVFCaps: u32,
    pub dwTextureOpCaps: u32,
    pub wMaxTextureBlendStages: u16,
    pub wMaxSimultaneousTextures: u16,
}
const _: () = assert!(core::mem::size_of::<D3DDEVICEDESC>() == 252);
const _: () = assert!(core::mem::align_of::<D3DDEVICEDESC>() == 4);
impl Default for D3DDEVICEDESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwFlags) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dcmColorModel) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwDevCaps) == 12);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dtcTransformCaps) == 16);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, bClipping) == 24);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dlcLightingCaps) == 28);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dpcLineCaps) == 44);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dpcTriCaps) == 100);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwDeviceRenderBitDepth) == 156);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwDeviceZBufferBitDepth) == 160);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxBufferSize) == 164);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxVertexCount) == 168);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMinTextureWidth) == 172);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMinTextureHeight) == 176);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxTextureWidth) == 180);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxTextureHeight) == 184);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMinStippleWidth) == 188);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxStippleWidth) == 192);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMinStippleHeight) == 196);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxStippleHeight) == 200);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxTextureRepeat) == 204);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxTextureAspectRatio) == 208);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwMaxAnisotropy) == 212);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dvGuardBandLeft) == 216);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dvGuardBandTop) == 220);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dvGuardBandRight) == 224);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dvGuardBandBottom) == 228);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dvExtentsAdjust) == 232);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwStencilCaps) == 236);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwFVFCaps) == 240);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, dwTextureOpCaps) == 244);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, wMaxTextureBlendStages) == 248);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC, wMaxSimultaneousTextures) == 250);
impl crate::GuestArg for D3DDEVICEDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 63;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDEVICEDESC7 {
    pub dwDevCaps: u32,
    pub dpcLineCaps: D3DPRIMCAPS,
    pub dpcTriCaps: D3DPRIMCAPS,
    pub dwDeviceRenderBitDepth: u32,
    pub dwDeviceZBufferBitDepth: u32,
    pub dwMinTextureWidth: u32,
    pub dwMinTextureHeight: u32,
    pub dwMaxTextureWidth: u32,
    pub dwMaxTextureHeight: u32,
    pub dwMaxTextureRepeat: u32,
    pub dwMaxTextureAspectRatio: u32,
    pub dwMaxAnisotropy: u32,
    pub dvGuardBandLeft: f32,
    pub dvGuardBandTop: f32,
    pub dvGuardBandRight: f32,
    pub dvGuardBandBottom: f32,
    pub dvExtentsAdjust: f32,
    pub dwStencilCaps: u32,
    pub dwFVFCaps: u32,
    pub dwTextureOpCaps: u32,
    pub wMaxTextureBlendStages: u16,
    pub wMaxSimultaneousTextures: u16,
    pub dwMaxActiveLights: u32,
    pub dvMaxVertexW: f32,
    pub deviceGUID: [u32; 4],
    pub wMaxUserClipPlanes: u16,
    pub wMaxVertexBlendMatrices: u16,
    pub dwVertexProcessingCaps: u32,
    pub dwReserved1: u32,
    pub dwReserved2: u32,
    pub dwReserved3: u32,
    pub dwReserved4: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDEVICEDESC7>() == 236);
const _: () = assert!(core::mem::align_of::<D3DDEVICEDESC7>() == 4);
impl Default for D3DDEVICEDESC7 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwDevCaps) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dpcLineCaps) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dpcTriCaps) == 60);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwDeviceRenderBitDepth) == 116);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwDeviceZBufferBitDepth) == 120);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMinTextureWidth) == 124);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMinTextureHeight) == 128);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxTextureWidth) == 132);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxTextureHeight) == 136);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxTextureRepeat) == 140);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxTextureAspectRatio) == 144);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxAnisotropy) == 148);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvGuardBandLeft) == 152);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvGuardBandTop) == 156);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvGuardBandRight) == 160);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvGuardBandBottom) == 164);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvExtentsAdjust) == 168);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwStencilCaps) == 172);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwFVFCaps) == 176);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwTextureOpCaps) == 180);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, wMaxTextureBlendStages) == 184);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, wMaxSimultaneousTextures) == 186);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwMaxActiveLights) == 188);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dvMaxVertexW) == 192);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, deviceGUID) == 196);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, wMaxUserClipPlanes) == 212);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, wMaxVertexBlendMatrices) == 214);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwVertexProcessingCaps) == 216);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwReserved1) == 220);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwReserved2) == 224);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwReserved3) == 228);
const _: () = assert!(core::mem::offset_of!(D3DDEVICEDESC7, dwReserved4) == 232);
impl crate::GuestArg for D3DDEVICEDESC7 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 59;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDP_PTRSTRIDE {
    pub lpvData: u32,
    pub dwStride: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDP_PTRSTRIDE>() == 8);
const _: () = assert!(core::mem::align_of::<D3DDP_PTRSTRIDE>() == 4);
impl Default for D3DDP_PTRSTRIDE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DDP_PTRSTRIDE, lpvData) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDP_PTRSTRIDE, dwStride) == 4);
impl crate::GuestArg for D3DDP_PTRSTRIDE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDRAWPRIMITIVESTRIDEDDATA {
    pub position: D3DDP_PTRSTRIDE,
    pub normal: D3DDP_PTRSTRIDE,
    pub diffuse: D3DDP_PTRSTRIDE,
    pub specular: D3DDP_PTRSTRIDE,
    pub textureCoords: [D3DDP_PTRSTRIDE; 8],
}
const _: () = assert!(core::mem::size_of::<D3DDRAWPRIMITIVESTRIDEDDATA>() == 96);
const _: () = assert!(core::mem::align_of::<D3DDRAWPRIMITIVESTRIDEDDATA>() == 4);
impl Default for D3DDRAWPRIMITIVESTRIDEDDATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DDRAWPRIMITIVESTRIDEDDATA, position) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDRAWPRIMITIVESTRIDEDDATA, normal) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDRAWPRIMITIVESTRIDEDDATA, diffuse) == 16);
const _: () = assert!(core::mem::offset_of!(D3DDRAWPRIMITIVESTRIDEDDATA, specular) == 24);
const _: () = assert!(core::mem::offset_of!(D3DDRAWPRIMITIVESTRIDEDDATA, textureCoords) == 32);
impl crate::GuestArg for D3DDRAWPRIMITIVESTRIDEDDATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 24;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DEXECUTEBUFFERDESC {
    pub dwSize: u32,
    pub dwFlags: u32,
    pub dwCaps: u32,
    pub dwBufferSize: u32,
    pub lpData: u32,
}
const _: () = assert!(core::mem::size_of::<D3DEXECUTEBUFFERDESC>() == 20);
const _: () = assert!(core::mem::align_of::<D3DEXECUTEBUFFERDESC>() == 4);
impl Default for D3DEXECUTEBUFFERDESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEBUFFERDESC, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEBUFFERDESC, dwFlags) == 4);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEBUFFERDESC, dwCaps) == 8);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEBUFFERDESC, dwBufferSize) == 12);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEBUFFERDESC, lpData) == 16);
impl crate::GuestArg for D3DEXECUTEBUFFERDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DEXECUTEDATA {
    pub dwSize: u32,
    pub dwVertexOffset: u32,
    pub dwVertexCount: u32,
    pub dwInstructionOffset: u32,
    pub dwInstructionLength: u32,
    pub dwHVertexOffset: u32,
    pub dsStatus: D3DSTATUS,
}
const _: () = assert!(core::mem::size_of::<D3DEXECUTEDATA>() == 48);
const _: () = assert!(core::mem::align_of::<D3DEXECUTEDATA>() == 4);
impl Default for D3DEXECUTEDATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwVertexOffset) == 4);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwVertexCount) == 8);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwInstructionOffset) == 12);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwInstructionLength) == 16);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dwHVertexOffset) == 20);
const _: () = assert!(core::mem::offset_of!(D3DEXECUTEDATA, dsStatus) == 24);
impl crate::GuestArg for D3DEXECUTEDATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 12;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DFINDDEVICERESULT {
    pub dwSize: u32,
    pub guid: [u32; 4],
    pub ddHwDesc: D3DDEVICEDESC,
    pub ddSwDesc: D3DDEVICEDESC,
}
const _: () = assert!(core::mem::size_of::<D3DFINDDEVICERESULT>() == 524);
const _: () = assert!(core::mem::align_of::<D3DFINDDEVICERESULT>() == 4);
impl Default for D3DFINDDEVICERESULT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICERESULT, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICERESULT, guid) == 4);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICERESULT, ddHwDesc) == 20);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICERESULT, ddSwDesc) == 272);
impl crate::GuestArg for D3DFINDDEVICERESULT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 131;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DFINDDEVICESEARCH {
    pub dwSize: u32,
    pub dwFlags: u32,
    pub bHardware: i32,
    pub dcmColorModel: u32,
    pub guid: [u32; 4],
    pub dwCaps: u32,
    pub dpcPrimCaps: D3DPRIMCAPS,
}
const _: () = assert!(core::mem::size_of::<D3DFINDDEVICESEARCH>() == 92);
const _: () = assert!(core::mem::align_of::<D3DFINDDEVICESEARCH>() == 4);
impl Default for D3DFINDDEVICESEARCH { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, dwFlags) == 4);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, bHardware) == 8);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, dcmColorModel) == 12);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, guid) == 16);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, dwCaps) == 32);
const _: () = assert!(core::mem::offset_of!(D3DFINDDEVICESEARCH, dpcPrimCaps) == 36);
impl crate::GuestArg for D3DFINDDEVICESEARCH {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 23;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DHVERTEX {
    pub dwFlags: u32,
    pub hx: f32,
    pub hy: f32,
    pub hz: f32,
}
const _: () = assert!(core::mem::size_of::<D3DHVERTEX>() == 16);
const _: () = assert!(core::mem::align_of::<D3DHVERTEX>() == 4);
impl Default for D3DHVERTEX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DHVERTEX, dwFlags) == 0);
const _: () = assert!(core::mem::offset_of!(D3DHVERTEX, hx) == 4);
const _: () = assert!(core::mem::offset_of!(D3DHVERTEX, hy) == 8);
const _: () = assert!(core::mem::offset_of!(D3DHVERTEX, hz) == 12);
impl crate::GuestArg for D3DHVERTEX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DINSTRUCTION {
    pub bOpcode: u8,
    pub bSize: u8,
    pub wCount: u16,
}
const _: () = assert!(core::mem::size_of::<D3DINSTRUCTION>() == 4);
const _: () = assert!(core::mem::align_of::<D3DINSTRUCTION>() == 2);
impl Default for D3DINSTRUCTION { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DINSTRUCTION, bOpcode) == 0);
const _: () = assert!(core::mem::offset_of!(D3DINSTRUCTION, bSize) == 1);
const _: () = assert!(core::mem::offset_of!(D3DINSTRUCTION, wCount) == 2);
impl crate::GuestArg for D3DINSTRUCTION {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHT {
    pub dwSize: u32,
    pub dltType: i32,
    pub dcvColor: D3DCOLORVALUE,
    pub dvPosition: D3DVECTOR,
    pub dvDirection: D3DVECTOR,
    pub dvRange: f32,
    pub dvFalloff: f32,
    pub dvAttenuation0: f32,
    pub dvAttenuation1: f32,
    pub dvAttenuation2: f32,
    pub dvTheta: f32,
    pub dvPhi: f32,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHT>() == 76);
const _: () = assert!(core::mem::align_of::<D3DLIGHT>() == 4);
impl Default for D3DLIGHT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dltType) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dcvColor) == 8);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvPosition) == 24);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvDirection) == 36);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvRange) == 48);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvFalloff) == 52);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvAttenuation0) == 56);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvAttenuation1) == 60);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvAttenuation2) == 64);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvTheta) == 68);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT, dvPhi) == 72);
impl crate::GuestArg for D3DLIGHT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 19;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHT2 {
    pub dwSize: u32,
    pub dltType: i32,
    pub dcvColor: D3DCOLORVALUE,
    pub dvPosition: D3DVECTOR,
    pub dvDirection: D3DVECTOR,
    pub dvRange: f32,
    pub dvFalloff: f32,
    pub dvAttenuation0: f32,
    pub dvAttenuation1: f32,
    pub dvAttenuation2: f32,
    pub dvTheta: f32,
    pub dvPhi: f32,
    pub dwFlags: u32,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHT2>() == 80);
const _: () = assert!(core::mem::align_of::<D3DLIGHT2>() == 4);
impl Default for D3DLIGHT2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dltType) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dcvColor) == 8);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvPosition) == 24);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvDirection) == 36);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvRange) == 48);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvFalloff) == 52);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvAttenuation0) == 56);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvAttenuation1) == 60);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvAttenuation2) == 64);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvTheta) == 68);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dvPhi) == 72);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT2, dwFlags) == 76);
impl crate::GuestArg for D3DLIGHT2 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 20;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHT7 {
    pub dltType: i32,
    pub dcvDiffuse: D3DCOLORVALUE,
    pub dcvSpecular: D3DCOLORVALUE,
    pub dcvAmbient: D3DCOLORVALUE,
    pub dvPosition: D3DVECTOR,
    pub dvDirection: D3DVECTOR,
    pub dvRange: f32,
    pub dvFalloff: f32,
    pub dvAttenuation0: f32,
    pub dvAttenuation1: f32,
    pub dvAttenuation2: f32,
    pub dvTheta: f32,
    pub dvPhi: f32,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHT7>() == 104);
const _: () = assert!(core::mem::align_of::<D3DLIGHT7>() == 4);
impl Default for D3DLIGHT7 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dltType) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dcvDiffuse) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dcvSpecular) == 20);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dcvAmbient) == 36);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvPosition) == 52);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvDirection) == 64);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvRange) == 76);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvFalloff) == 80);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvAttenuation0) == 84);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvAttenuation1) == 88);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvAttenuation2) == 92);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvTheta) == 96);
const _: () = assert!(core::mem::offset_of!(D3DLIGHT7, dvPhi) == 100);
impl crate::GuestArg for D3DLIGHT7 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 26;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHTDATA {
    pub dwSize: u32,
    pub lpIn: u32,
    pub dwInSize: u32,
    pub lpOut: u32,
    pub dwOutSize: u32,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHTDATA>() == 20);
const _: () = assert!(core::mem::align_of::<D3DLIGHTDATA>() == 4);
impl Default for D3DLIGHTDATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHTDATA, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTDATA, lpIn) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTDATA, dwInSize) == 8);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTDATA, lpOut) == 12);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTDATA, dwOutSize) == 16);
impl crate::GuestArg for D3DLIGHTDATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHTINGCAPS {
    pub dwSize: u32,
    pub dwCaps: u32,
    pub dwLightingModel: u32,
    pub dwNumLights: u32,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHTINGCAPS>() == 16);
const _: () = assert!(core::mem::align_of::<D3DLIGHTINGCAPS>() == 4);
impl Default for D3DLIGHTINGCAPS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGCAPS, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGCAPS, dwCaps) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGCAPS, dwLightingModel) == 8);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGCAPS, dwNumLights) == 12);
impl crate::GuestArg for D3DLIGHTINGCAPS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLIGHTINGELEMENT {
    pub dvPosition: D3DVECTOR,
    pub dvNormal: D3DVECTOR,
}
const _: () = assert!(core::mem::size_of::<D3DLIGHTINGELEMENT>() == 24);
const _: () = assert!(core::mem::align_of::<D3DLIGHTINGELEMENT>() == 4);
impl Default for D3DLIGHTINGELEMENT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGELEMENT, dvPosition) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLIGHTINGELEMENT, dvNormal) == 12);
impl crate::GuestArg for D3DLIGHTINGELEMENT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLINE {
    pub v1: u16,
    pub v2: u16,
}
const _: () = assert!(core::mem::size_of::<D3DLINE>() == 4);
const _: () = assert!(core::mem::align_of::<D3DLINE>() == 2);
impl Default for D3DLINE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLINE, v1) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLINE, v2) == 2);
impl crate::GuestArg for D3DLINE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLVERTEX {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub dwReserved: u32,
    pub color: u32,
    pub specular: u32,
    pub tu: f32,
    pub tv: f32,
}
const _: () = assert!(core::mem::size_of::<D3DLVERTEX>() == 32);
const _: () = assert!(core::mem::align_of::<D3DLVERTEX>() == 4);
impl Default for D3DLVERTEX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, x) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, y) == 4);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, z) == 8);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, dwReserved) == 12);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, color) == 16);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, specular) == 20);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, tu) == 24);
const _: () = assert!(core::mem::offset_of!(D3DLVERTEX, tv) == 28);
impl crate::GuestArg for D3DLVERTEX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DMATERIAL {
    pub dwSize: u32,
    pub diffuse: D3DCOLORVALUE,
    pub ambient: D3DCOLORVALUE,
    pub specular: D3DCOLORVALUE,
    pub emissive: D3DCOLORVALUE,
    pub power: f32,
    pub hTexture: u32,
    pub dwRampSize: u32,
}
const _: () = assert!(core::mem::size_of::<D3DMATERIAL>() == 80);
const _: () = assert!(core::mem::align_of::<D3DMATERIAL>() == 4);
impl Default for D3DMATERIAL { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, diffuse) == 4);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, ambient) == 20);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, specular) == 36);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, emissive) == 52);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, power) == 68);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, hTexture) == 72);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL, dwRampSize) == 76);
impl crate::GuestArg for D3DMATERIAL {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 20;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DMATERIAL7 {
    pub diffuse: D3DCOLORVALUE,
    pub ambient: D3DCOLORVALUE,
    pub specular: D3DCOLORVALUE,
    pub emissive: D3DCOLORVALUE,
    pub power: f32,
}
const _: () = assert!(core::mem::size_of::<D3DMATERIAL7>() == 68);
const _: () = assert!(core::mem::align_of::<D3DMATERIAL7>() == 4);
impl Default for D3DMATERIAL7 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL7, diffuse) == 0);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL7, ambient) == 16);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL7, specular) == 32);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL7, emissive) == 48);
const _: () = assert!(core::mem::offset_of!(D3DMATERIAL7, power) == 64);
impl crate::GuestArg for D3DMATERIAL7 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 17;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DMATRIX {
    pub Anonymous: [u8; 64],
}
const _: () = assert!(core::mem::size_of::<D3DMATRIX>() == 64);
const _: () = assert!(core::mem::align_of::<D3DMATRIX>() == 4);
impl Default for D3DMATRIX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DMATRIX, Anonymous) == 0);
impl crate::GuestArg for D3DMATRIX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 16;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DMATRIXLOAD {
    pub hDestMatrix: u32,
    pub hSrcMatrix: u32,
}
const _: () = assert!(core::mem::size_of::<D3DMATRIXLOAD>() == 8);
const _: () = assert!(core::mem::align_of::<D3DMATRIXLOAD>() == 4);
impl Default for D3DMATRIXLOAD { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DMATRIXLOAD, hDestMatrix) == 0);
const _: () = assert!(core::mem::offset_of!(D3DMATRIXLOAD, hSrcMatrix) == 4);
impl crate::GuestArg for D3DMATRIXLOAD {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DMATRIXMULTIPLY {
    pub hDestMatrix: u32,
    pub hSrcMatrix1: u32,
    pub hSrcMatrix2: u32,
}
const _: () = assert!(core::mem::size_of::<D3DMATRIXMULTIPLY>() == 12);
const _: () = assert!(core::mem::align_of::<D3DMATRIXMULTIPLY>() == 4);
impl Default for D3DMATRIXMULTIPLY { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DMATRIXMULTIPLY, hDestMatrix) == 0);
const _: () = assert!(core::mem::offset_of!(D3DMATRIXMULTIPLY, hSrcMatrix1) == 4);
const _: () = assert!(core::mem::offset_of!(D3DMATRIXMULTIPLY, hSrcMatrix2) == 8);
impl crate::GuestArg for D3DMATRIXMULTIPLY {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPICKRECORD {
    pub bOpcode: u8,
    pub bPad: u8,
    pub _padding0: [u8; 2],
    pub dwOffset: u32,
    pub dvZ: f32,
}
const _: () = assert!(core::mem::size_of::<D3DPICKRECORD>() == 12);
const _: () = assert!(core::mem::align_of::<D3DPICKRECORD>() == 4);
impl Default for D3DPICKRECORD { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DPICKRECORD, bOpcode) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPICKRECORD, bPad) == 1);
const _: () = assert!(core::mem::offset_of!(D3DPICKRECORD, dwOffset) == 4);
const _: () = assert!(core::mem::offset_of!(D3DPICKRECORD, dvZ) == 8);
impl crate::GuestArg for D3DPICKRECORD {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPOINT {
    pub wCount: u16,
    pub wFirst: u16,
}
const _: () = assert!(core::mem::size_of::<D3DPOINT>() == 4);
const _: () = assert!(core::mem::align_of::<D3DPOINT>() == 2);
impl Default for D3DPOINT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DPOINT, wCount) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPOINT, wFirst) == 2);
impl crate::GuestArg for D3DPOINT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPRIMCAPS {
    pub dwSize: u32,
    pub dwMiscCaps: u32,
    pub dwRasterCaps: u32,
    pub dwZCmpCaps: u32,
    pub dwSrcBlendCaps: u32,
    pub dwDestBlendCaps: u32,
    pub dwAlphaCmpCaps: u32,
    pub dwShadeCaps: u32,
    pub dwTextureCaps: u32,
    pub dwTextureFilterCaps: u32,
    pub dwTextureBlendCaps: u32,
    pub dwTextureAddressCaps: u32,
    pub dwStippleWidth: u32,
    pub dwStippleHeight: u32,
}
const _: () = assert!(core::mem::size_of::<D3DPRIMCAPS>() == 56);
const _: () = assert!(core::mem::align_of::<D3DPRIMCAPS>() == 4);
impl Default for D3DPRIMCAPS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwMiscCaps) == 4);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwRasterCaps) == 8);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwZCmpCaps) == 12);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwSrcBlendCaps) == 16);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwDestBlendCaps) == 20);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwAlphaCmpCaps) == 24);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwShadeCaps) == 28);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwTextureCaps) == 32);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwTextureFilterCaps) == 36);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwTextureBlendCaps) == 40);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwTextureAddressCaps) == 44);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwStippleWidth) == 48);
const _: () = assert!(core::mem::offset_of!(D3DPRIMCAPS, dwStippleHeight) == 52);
impl crate::GuestArg for D3DPRIMCAPS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 14;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DPROCESSVERTICES {
    pub dwFlags: u32,
    pub wStart: u16,
    pub wDest: u16,
    pub dwCount: u32,
    pub dwReserved: u32,
}
const _: () = assert!(core::mem::size_of::<D3DPROCESSVERTICES>() == 16);
const _: () = assert!(core::mem::align_of::<D3DPROCESSVERTICES>() == 4);
impl Default for D3DPROCESSVERTICES { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DPROCESSVERTICES, dwFlags) == 0);
const _: () = assert!(core::mem::offset_of!(D3DPROCESSVERTICES, wStart) == 4);
const _: () = assert!(core::mem::offset_of!(D3DPROCESSVERTICES, wDest) == 6);
const _: () = assert!(core::mem::offset_of!(D3DPROCESSVERTICES, dwCount) == 8);
const _: () = assert!(core::mem::offset_of!(D3DPROCESSVERTICES, dwReserved) == 12);
impl crate::GuestArg for D3DPROCESSVERTICES {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DRECT {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}
const _: () = assert!(core::mem::size_of::<D3DRECT>() == 16);
const _: () = assert!(core::mem::align_of::<D3DRECT>() == 4);
impl Default for D3DRECT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DRECT, x1) == 0);
const _: () = assert!(core::mem::offset_of!(D3DRECT, y1) == 4);
const _: () = assert!(core::mem::offset_of!(D3DRECT, x2) == 8);
const _: () = assert!(core::mem::offset_of!(D3DRECT, y2) == 12);
impl crate::GuestArg for D3DRECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DSPAN {
    pub wCount: u16,
    pub wFirst: u16,
}
const _: () = assert!(core::mem::size_of::<D3DSPAN>() == 4);
const _: () = assert!(core::mem::align_of::<D3DSPAN>() == 2);
impl Default for D3DSPAN { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DSPAN, wCount) == 0);
const _: () = assert!(core::mem::offset_of!(D3DSPAN, wFirst) == 2);
impl crate::GuestArg for D3DSPAN {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DSTATE {
    pub Anonymous1: [u8; 4],
    pub Anonymous2: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<D3DSTATE>() == 8);
const _: () = assert!(core::mem::align_of::<D3DSTATE>() == 4);
impl Default for D3DSTATE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DSTATE, Anonymous1) == 0);
const _: () = assert!(core::mem::offset_of!(D3DSTATE, Anonymous2) == 4);
impl crate::GuestArg for D3DSTATE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DSTATS {
    pub dwSize: u32,
    pub dwTrianglesDrawn: u32,
    pub dwLinesDrawn: u32,
    pub dwPointsDrawn: u32,
    pub dwSpansDrawn: u32,
    pub dwVerticesProcessed: u32,
}
const _: () = assert!(core::mem::size_of::<D3DSTATS>() == 24);
const _: () = assert!(core::mem::align_of::<D3DSTATS>() == 4);
impl Default for D3DSTATS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwTrianglesDrawn) == 4);
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwLinesDrawn) == 8);
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwPointsDrawn) == 12);
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwSpansDrawn) == 16);
const _: () = assert!(core::mem::offset_of!(D3DSTATS, dwVerticesProcessed) == 20);
impl crate::GuestArg for D3DSTATS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DSTATUS {
    pub dwFlags: u32,
    pub dwStatus: u32,
    pub drExtent: D3DRECT,
}
const _: () = assert!(core::mem::size_of::<D3DSTATUS>() == 24);
const _: () = assert!(core::mem::align_of::<D3DSTATUS>() == 4);
impl Default for D3DSTATUS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DSTATUS, dwFlags) == 0);
const _: () = assert!(core::mem::offset_of!(D3DSTATUS, dwStatus) == 4);
const _: () = assert!(core::mem::offset_of!(D3DSTATUS, drExtent) == 8);
impl crate::GuestArg for D3DSTATUS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DTEXTURELOAD {
    pub hDestTexture: u32,
    pub hSrcTexture: u32,
}
const _: () = assert!(core::mem::size_of::<D3DTEXTURELOAD>() == 8);
const _: () = assert!(core::mem::align_of::<D3DTEXTURELOAD>() == 4);
impl Default for D3DTEXTURELOAD { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DTEXTURELOAD, hDestTexture) == 0);
const _: () = assert!(core::mem::offset_of!(D3DTEXTURELOAD, hSrcTexture) == 4);
impl crate::GuestArg for D3DTEXTURELOAD {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DTLVERTEX {
    pub sx: f32,
    pub sy: f32,
    pub sz: f32,
    pub rhw: f32,
    pub color: u32,
    pub specular: u32,
    pub tu: f32,
    pub tv: f32,
}
const _: () = assert!(core::mem::size_of::<D3DTLVERTEX>() == 32);
const _: () = assert!(core::mem::align_of::<D3DTLVERTEX>() == 4);
impl Default for D3DTLVERTEX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, sx) == 0);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, sy) == 4);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, sz) == 8);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, rhw) == 12);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, color) == 16);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, specular) == 20);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, tu) == 24);
const _: () = assert!(core::mem::offset_of!(D3DTLVERTEX, tv) == 28);
impl crate::GuestArg for D3DTLVERTEX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DTRANSFORMCAPS {
    pub dwSize: u32,
    pub dwCaps: u32,
}
const _: () = assert!(core::mem::size_of::<D3DTRANSFORMCAPS>() == 8);
const _: () = assert!(core::mem::align_of::<D3DTRANSFORMCAPS>() == 4);
impl Default for D3DTRANSFORMCAPS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMCAPS, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMCAPS, dwCaps) == 4);
impl crate::GuestArg for D3DTRANSFORMCAPS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DTRANSFORMDATA {
    pub dwSize: u32,
    pub lpIn: u32,
    pub dwInSize: u32,
    pub lpOut: u32,
    pub dwOutSize: u32,
    pub lpHOut: u32,
    pub dwClip: u32,
    pub dwClipIntersection: u32,
    pub dwClipUnion: u32,
    pub drExtent: D3DRECT,
}
const _: () = assert!(core::mem::size_of::<D3DTRANSFORMDATA>() == 52);
const _: () = assert!(core::mem::align_of::<D3DTRANSFORMDATA>() == 4);
impl Default for D3DTRANSFORMDATA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, lpIn) == 4);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwInSize) == 8);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, lpOut) == 12);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwOutSize) == 16);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, lpHOut) == 20);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwClip) == 24);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwClipIntersection) == 28);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, dwClipUnion) == 32);
const _: () = assert!(core::mem::offset_of!(D3DTRANSFORMDATA, drExtent) == 36);
impl crate::GuestArg for D3DTRANSFORMDATA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 13;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(2))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DTRIANGLE {
    pub v1: u16,
    pub v2: u16,
    pub v3: u16,
    pub wFlags: u16,
}
const _: () = assert!(core::mem::size_of::<D3DTRIANGLE>() == 8);
const _: () = assert!(core::mem::align_of::<D3DTRIANGLE>() == 2);
impl Default for D3DTRIANGLE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DTRIANGLE, v1) == 0);
const _: () = assert!(core::mem::offset_of!(D3DTRIANGLE, v2) == 2);
const _: () = assert!(core::mem::offset_of!(D3DTRIANGLE, v3) == 4);
const _: () = assert!(core::mem::offset_of!(D3DTRIANGLE, wFlags) == 6);
impl crate::GuestArg for D3DTRIANGLE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVECTOR {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVECTOR>() == 12);
const _: () = assert!(core::mem::align_of::<D3DVECTOR>() == 4);
impl Default for D3DVECTOR { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVECTOR, x) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVECTOR, y) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVECTOR, z) == 8);
impl crate::GuestArg for D3DVECTOR {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVERTEX {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub nx: f32,
    pub ny: f32,
    pub nz: f32,
    pub tu: f32,
    pub tv: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVERTEX>() == 32);
const _: () = assert!(core::mem::align_of::<D3DVERTEX>() == 4);
impl Default for D3DVERTEX { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, x) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, y) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, z) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, nx) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, ny) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, nz) == 20);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, tu) == 24);
const _: () = assert!(core::mem::offset_of!(D3DVERTEX, tv) == 28);
impl crate::GuestArg for D3DVERTEX {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVERTEXBUFFERDESC {
    pub dwSize: u32,
    pub dwCaps: u32,
    pub dwFVF: u32,
    pub dwNumVertices: u32,
}
const _: () = assert!(core::mem::size_of::<D3DVERTEXBUFFERDESC>() == 16);
const _: () = assert!(core::mem::align_of::<D3DVERTEXBUFFERDESC>() == 4);
impl Default for D3DVERTEXBUFFERDESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFERDESC, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFERDESC, dwCaps) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFERDESC, dwFVF) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVERTEXBUFFERDESC, dwNumVertices) == 12);
impl crate::GuestArg for D3DVERTEXBUFFERDESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVIEWPORT {
    pub dwSize: u32,
    pub dwX: u32,
    pub dwY: u32,
    pub dwWidth: u32,
    pub dwHeight: u32,
    pub dvScaleX: f32,
    pub dvScaleY: f32,
    pub dvMaxX: f32,
    pub dvMaxY: f32,
    pub dvMinZ: f32,
    pub dvMaxZ: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVIEWPORT>() == 44);
const _: () = assert!(core::mem::align_of::<D3DVIEWPORT>() == 4);
impl Default for D3DVIEWPORT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dwX) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dwY) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dwWidth) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dwHeight) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvScaleX) == 20);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvScaleY) == 24);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvMaxX) == 28);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvMaxY) == 32);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvMinZ) == 36);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT, dvMaxZ) == 40);
impl crate::GuestArg for D3DVIEWPORT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVIEWPORT2 {
    pub dwSize: u32,
    pub dwX: u32,
    pub dwY: u32,
    pub dwWidth: u32,
    pub dwHeight: u32,
    pub dvClipX: f32,
    pub dvClipY: f32,
    pub dvClipWidth: f32,
    pub dvClipHeight: f32,
    pub dvMinZ: f32,
    pub dvMaxZ: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVIEWPORT2>() == 44);
const _: () = assert!(core::mem::align_of::<D3DVIEWPORT2>() == 4);
impl Default for D3DVIEWPORT2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dwX) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dwY) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dwWidth) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dwHeight) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvClipX) == 20);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvClipY) == 24);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvClipWidth) == 28);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvClipHeight) == 32);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvMinZ) == 36);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT2, dvMaxZ) == 40);
impl crate::GuestArg for D3DVIEWPORT2 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DVIEWPORT7 {
    pub dwX: u32,
    pub dwY: u32,
    pub dwWidth: u32,
    pub dwHeight: u32,
    pub dvMinZ: f32,
    pub dvMaxZ: f32,
}
const _: () = assert!(core::mem::size_of::<D3DVIEWPORT7>() == 24);
const _: () = assert!(core::mem::align_of::<D3DVIEWPORT7>() == 4);
impl Default for D3DVIEWPORT7 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dwX) == 0);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dwY) == 4);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dwWidth) == 8);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dwHeight) == 12);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dvMinZ) == 16);
const _: () = assert!(core::mem::offset_of!(D3DVIEWPORT7, dvMaxZ) == 20);
impl crate::GuestArg for D3DVIEWPORT7 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DDPIXELFORMAT {
    pub dwSize: u32,
    pub dwFlags: u32,
    pub dwFourCC: u32,
    pub dwRGBBitCount: u32,
    pub dwRBitMask: u32,
    pub Anonymous3: [u8; 4],
    pub dwBBitMask: u32,
    pub dwRGBAlphaBitMask: u32,
}
const _: () = assert!(core::mem::size_of::<DDPIXELFORMAT>() == 32);
const _: () = assert!(core::mem::align_of::<DDPIXELFORMAT>() == 4);
impl Default for DDPIXELFORMAT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwSize) == 0);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwFlags) == 4);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwFourCC) == 8);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwRGBBitCount) == 12);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwRBitMask) == 16);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, Anonymous3) == 20);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwBBitMask) == 24);
const _: () = assert!(core::mem::offset_of!(DDPIXELFORMAT, dwRGBAlphaBitMask) == 28);
impl crate::GuestArg for DDPIXELFORMAT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_ADAPTER_DESC {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: u32,
    pub DedicatedSystemMemory: u32,
    pub SharedSystemMemory: u32,
    pub AdapterLuid: LUID,
}
const _: () = assert!(core::mem::size_of::<DXGI_ADAPTER_DESC>() == 292);
const _: () = assert!(core::mem::align_of::<DXGI_ADAPTER_DESC>() == 4);
impl Default for DXGI_ADAPTER_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, Description) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, VendorId) == 256);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, DeviceId) == 260);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, SubSysId) == 264);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, Revision) == 268);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, DedicatedVideoMemory) == 272);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, DedicatedSystemMemory) == 276);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, SharedSystemMemory) == 280);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC, AdapterLuid) == 284);
impl crate::GuestArg for DXGI_ADAPTER_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 73;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_ADAPTER_DESC1 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: u32,
    pub DedicatedSystemMemory: u32,
    pub SharedSystemMemory: u32,
    pub AdapterLuid: LUID,
    pub Flags: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_ADAPTER_DESC1>() == 296);
const _: () = assert!(core::mem::align_of::<DXGI_ADAPTER_DESC1>() == 4);
impl Default for DXGI_ADAPTER_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, Description) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, VendorId) == 256);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, DeviceId) == 260);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, SubSysId) == 264);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, Revision) == 268);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, DedicatedVideoMemory) == 272);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, DedicatedSystemMemory) == 276);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, SharedSystemMemory) == 280);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, AdapterLuid) == 284);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC1, Flags) == 292);
impl crate::GuestArg for DXGI_ADAPTER_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 74;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_ADAPTER_DESC2 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: u32,
    pub DedicatedSystemMemory: u32,
    pub SharedSystemMemory: u32,
    pub AdapterLuid: LUID,
    pub Flags: u32,
    pub GraphicsPreemptionGranularity: i32,
    pub ComputePreemptionGranularity: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_ADAPTER_DESC2>() == 304);
const _: () = assert!(core::mem::align_of::<DXGI_ADAPTER_DESC2>() == 4);
impl Default for DXGI_ADAPTER_DESC2 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, Description) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, VendorId) == 256);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, DeviceId) == 260);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, SubSysId) == 264);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, Revision) == 268);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, DedicatedVideoMemory) == 272);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, DedicatedSystemMemory) == 276);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, SharedSystemMemory) == 280);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, AdapterLuid) == 284);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, Flags) == 292);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, GraphicsPreemptionGranularity) == 296);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC2, ComputePreemptionGranularity) == 300);
impl crate::GuestArg for DXGI_ADAPTER_DESC2 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 76;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_ADAPTER_DESC3 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: u32,
    pub DedicatedSystemMemory: u32,
    pub SharedSystemMemory: u32,
    pub AdapterLuid: LUID,
    pub Flags: i32,
    pub GraphicsPreemptionGranularity: i32,
    pub ComputePreemptionGranularity: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_ADAPTER_DESC3>() == 304);
const _: () = assert!(core::mem::align_of::<DXGI_ADAPTER_DESC3>() == 4);
impl Default for DXGI_ADAPTER_DESC3 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, Description) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, VendorId) == 256);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, DeviceId) == 260);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, SubSysId) == 264);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, Revision) == 268);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, DedicatedVideoMemory) == 272);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, DedicatedSystemMemory) == 276);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, SharedSystemMemory) == 280);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, AdapterLuid) == 284);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, Flags) == 292);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, GraphicsPreemptionGranularity) == 296);
const _: () = assert!(core::mem::offset_of!(DXGI_ADAPTER_DESC3, ComputePreemptionGranularity) == 300);
impl crate::GuestArg for DXGI_ADAPTER_DESC3 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 76;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_FRAME_STATISTICS {
    pub PresentCount: u32,
    pub PresentRefreshCount: u32,
    pub SyncRefreshCount: u32,
    pub _padding0: [u8; 4],
    pub SyncQPCTime: i64,
    pub SyncGPUTime: i64,
}
const _: () = assert!(core::mem::size_of::<DXGI_FRAME_STATISTICS>() == 32);
const _: () = assert!(core::mem::align_of::<DXGI_FRAME_STATISTICS>() == 8);
impl Default for DXGI_FRAME_STATISTICS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_FRAME_STATISTICS, PresentCount) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_FRAME_STATISTICS, PresentRefreshCount) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_FRAME_STATISTICS, SyncRefreshCount) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_FRAME_STATISTICS, SyncQPCTime) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_FRAME_STATISTICS, SyncGPUTime) == 24);
impl crate::GuestArg for DXGI_FRAME_STATISTICS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_GAMMA_CONTROL {
    pub Scale: DXGI_RGB,
    pub Offset: DXGI_RGB,
    pub GammaCurve: [DXGI_RGB; 1025],
}
const _: () = assert!(core::mem::size_of::<DXGI_GAMMA_CONTROL>() == 12324);
const _: () = assert!(core::mem::align_of::<DXGI_GAMMA_CONTROL>() == 4);
impl Default for DXGI_GAMMA_CONTROL { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL, Scale) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL, Offset) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL, GammaCurve) == 24);
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_GAMMA_CONTROL_CAPABILITIES {
    pub ScaleAndOffsetSupported: i32,
    pub MaxConvertedValue: f32,
    pub MinConvertedValue: f32,
    pub NumGammaControlPoints: u32,
    pub ControlPointPositions: [f32; 1025],
}
const _: () = assert!(core::mem::size_of::<DXGI_GAMMA_CONTROL_CAPABILITIES>() == 4116);
const _: () = assert!(core::mem::align_of::<DXGI_GAMMA_CONTROL_CAPABILITIES>() == 4);
impl Default for DXGI_GAMMA_CONTROL_CAPABILITIES { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, ScaleAndOffsetSupported) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, MaxConvertedValue) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, MinConvertedValue) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, NumGammaControlPoints) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_GAMMA_CONTROL_CAPABILITIES, ControlPointPositions) == 16);
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_MAPPED_RECT {
    pub Pitch: i32,
    pub pBits: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_MAPPED_RECT>() == 8);
const _: () = assert!(core::mem::align_of::<DXGI_MAPPED_RECT>() == 4);
impl Default for DXGI_MAPPED_RECT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_MAPPED_RECT, Pitch) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_MAPPED_RECT, pBits) == 4);
impl crate::GuestArg for DXGI_MAPPED_RECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_MATRIX_3X2_F {
    pub _11: f32,
    pub _12: f32,
    pub _21: f32,
    pub _22: f32,
    pub _31: f32,
    pub _32: f32,
}
const _: () = assert!(core::mem::size_of::<DXGI_MATRIX_3X2_F>() == 24);
const _: () = assert!(core::mem::align_of::<DXGI_MATRIX_3X2_F>() == 4);
impl Default for DXGI_MATRIX_3X2_F { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _11) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _12) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _21) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _22) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _31) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_MATRIX_3X2_F, _32) == 20);
impl crate::GuestArg for DXGI_MATRIX_3X2_F {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_MODE_DESC {
    pub Width: u32,
    pub Height: u32,
    pub RefreshRate: DXGI_RATIONAL,
    pub Format: i32,
    pub ScanlineOrdering: i32,
    pub Scaling: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_MODE_DESC>() == 28);
const _: () = assert!(core::mem::align_of::<DXGI_MODE_DESC>() == 4);
impl Default for DXGI_MODE_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, RefreshRate) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, Format) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, ScanlineOrdering) == 20);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC, Scaling) == 24);
impl crate::GuestArg for DXGI_MODE_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 7;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_MODE_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub RefreshRate: DXGI_RATIONAL,
    pub Format: i32,
    pub ScanlineOrdering: i32,
    pub Scaling: i32,
    pub Stereo: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_MODE_DESC1>() == 32);
const _: () = assert!(core::mem::align_of::<DXGI_MODE_DESC1>() == 4);
impl Default for DXGI_MODE_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, Width) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, Height) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, RefreshRate) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, Format) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, ScanlineOrdering) == 20);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, Scaling) == 24);
const _: () = assert!(core::mem::offset_of!(DXGI_MODE_DESC1, Stereo) == 28);
impl crate::GuestArg for DXGI_MODE_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTDUPL_DESC {
    pub ModeDesc: DXGI_MODE_DESC,
    pub Rotation: i32,
    pub DesktopImageInSystemMemory: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTDUPL_DESC>() == 36);
const _: () = assert!(core::mem::align_of::<DXGI_OUTDUPL_DESC>() == 4);
impl Default for DXGI_OUTDUPL_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_DESC, ModeDesc) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_DESC, Rotation) == 28);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_DESC, DesktopImageInSystemMemory) == 32);
impl crate::GuestArg for DXGI_OUTDUPL_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 9;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTDUPL_FRAME_INFO {
    pub LastPresentTime: i64,
    pub LastMouseUpdateTime: i64,
    pub AccumulatedFrames: u32,
    pub RectsCoalesced: i32,
    pub ProtectedContentMaskedOut: i32,
    pub PointerPosition: DXGI_OUTDUPL_POINTER_POSITION,
    pub TotalMetadataBufferSize: u32,
    pub PointerShapeBufferSize: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTDUPL_FRAME_INFO>() == 48);
const _: () = assert!(core::mem::align_of::<DXGI_OUTDUPL_FRAME_INFO>() == 8);
impl Default for DXGI_OUTDUPL_FRAME_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, LastPresentTime) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, LastMouseUpdateTime) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, AccumulatedFrames) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, RectsCoalesced) == 20);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, ProtectedContentMaskedOut) == 24);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, PointerPosition) == 28);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, TotalMetadataBufferSize) == 40);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_FRAME_INFO, PointerShapeBufferSize) == 44);
impl crate::GuestArg for DXGI_OUTDUPL_FRAME_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 12;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTDUPL_MOVE_RECT {
    pub SourcePoint: POINT,
    pub DestinationRect: RECT,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>() == 24);
const _: () = assert!(core::mem::align_of::<DXGI_OUTDUPL_MOVE_RECT>() == 4);
impl Default for DXGI_OUTDUPL_MOVE_RECT { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_MOVE_RECT, SourcePoint) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_MOVE_RECT, DestinationRect) == 8);
impl crate::GuestArg for DXGI_OUTDUPL_MOVE_RECT {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTDUPL_POINTER_POSITION {
    pub Position: POINT,
    pub Visible: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTDUPL_POINTER_POSITION>() == 12);
const _: () = assert!(core::mem::align_of::<DXGI_OUTDUPL_POINTER_POSITION>() == 4);
impl Default for DXGI_OUTDUPL_POINTER_POSITION { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_POSITION, Position) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_POSITION, Visible) == 8);
impl crate::GuestArg for DXGI_OUTDUPL_POINTER_POSITION {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTDUPL_POINTER_SHAPE_INFO {
    pub Type: u32,
    pub Width: u32,
    pub Height: u32,
    pub Pitch: u32,
    pub HotSpot: POINT,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTDUPL_POINTER_SHAPE_INFO>() == 24);
const _: () = assert!(core::mem::align_of::<DXGI_OUTDUPL_POINTER_SHAPE_INFO>() == 4);
impl Default for DXGI_OUTDUPL_POINTER_SHAPE_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Type) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Width) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Height) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Pitch) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTDUPL_POINTER_SHAPE_INFO, HotSpot) == 16);
impl crate::GuestArg for DXGI_OUTDUPL_POINTER_SHAPE_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 6;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTPUT_DESC {
    pub DeviceName: [u16; 32],
    pub DesktopCoordinates: RECT,
    pub AttachedToDesktop: i32,
    pub Rotation: i32,
    pub Monitor: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTPUT_DESC>() == 92);
const _: () = assert!(core::mem::align_of::<DXGI_OUTPUT_DESC>() == 4);
impl Default for DXGI_OUTPUT_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC, DeviceName) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC, DesktopCoordinates) == 64);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC, AttachedToDesktop) == 80);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC, Rotation) == 84);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC, Monitor) == 88);
impl crate::GuestArg for DXGI_OUTPUT_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 23;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_OUTPUT_DESC1 {
    pub DeviceName: [u16; 32],
    pub DesktopCoordinates: RECT,
    pub AttachedToDesktop: i32,
    pub Rotation: i32,
    pub Monitor: u32,
    pub BitsPerColor: u32,
    pub ColorSpace: i32,
    pub RedPrimary: [f32; 2],
    pub GreenPrimary: [f32; 2],
    pub BluePrimary: [f32; 2],
    pub WhitePoint: [f32; 2],
    pub MinLuminance: f32,
    pub MaxLuminance: f32,
    pub MaxFullFrameLuminance: f32,
}
const _: () = assert!(core::mem::size_of::<DXGI_OUTPUT_DESC1>() == 144);
const _: () = assert!(core::mem::align_of::<DXGI_OUTPUT_DESC1>() == 4);
impl Default for DXGI_OUTPUT_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, DeviceName) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, DesktopCoordinates) == 64);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, AttachedToDesktop) == 80);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, Rotation) == 84);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, Monitor) == 88);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, BitsPerColor) == 92);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, ColorSpace) == 96);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, RedPrimary) == 100);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, GreenPrimary) == 108);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, BluePrimary) == 116);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, WhitePoint) == 124);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, MinLuminance) == 132);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, MaxLuminance) == 136);
const _: () = assert!(core::mem::offset_of!(DXGI_OUTPUT_DESC1, MaxFullFrameLuminance) == 140);
impl crate::GuestArg for DXGI_OUTPUT_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 36;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_PRESENT_PARAMETERS {
    pub DirtyRectsCount: u32,
    pub pDirtyRects: u32,
    pub pScrollRect: u32,
    pub pScrollOffset: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_PRESENT_PARAMETERS>() == 16);
const _: () = assert!(core::mem::align_of::<DXGI_PRESENT_PARAMETERS>() == 4);
impl Default for DXGI_PRESENT_PARAMETERS { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_PRESENT_PARAMETERS, DirtyRectsCount) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_PRESENT_PARAMETERS, pDirtyRects) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_PRESENT_PARAMETERS, pScrollRect) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_PRESENT_PARAMETERS, pScrollOffset) == 12);
impl crate::GuestArg for DXGI_PRESENT_PARAMETERS {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_QUERY_VIDEO_MEMORY_INFO {
    pub Budget: u64,
    pub CurrentUsage: u64,
    pub AvailableForReservation: u64,
    pub CurrentReservation: u64,
}
const _: () = assert!(core::mem::size_of::<DXGI_QUERY_VIDEO_MEMORY_INFO>() == 32);
const _: () = assert!(core::mem::align_of::<DXGI_QUERY_VIDEO_MEMORY_INFO>() == 8);
impl Default for DXGI_QUERY_VIDEO_MEMORY_INFO { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_QUERY_VIDEO_MEMORY_INFO, Budget) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_QUERY_VIDEO_MEMORY_INFO, CurrentUsage) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_QUERY_VIDEO_MEMORY_INFO, AvailableForReservation) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_QUERY_VIDEO_MEMORY_INFO, CurrentReservation) == 24);
impl crate::GuestArg for DXGI_QUERY_VIDEO_MEMORY_INFO {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 8;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_RATIONAL {
    pub Numerator: u32,
    pub Denominator: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_RATIONAL>() == 8);
const _: () = assert!(core::mem::align_of::<DXGI_RATIONAL>() == 4);
impl Default for DXGI_RATIONAL { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_RATIONAL, Numerator) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_RATIONAL, Denominator) == 4);
impl crate::GuestArg for DXGI_RATIONAL {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_RGB {
    pub Red: f32,
    pub Green: f32,
    pub Blue: f32,
}
const _: () = assert!(core::mem::size_of::<DXGI_RGB>() == 12);
const _: () = assert!(core::mem::align_of::<DXGI_RGB>() == 4);
impl Default for DXGI_RGB { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_RGB, Red) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_RGB, Green) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_RGB, Blue) == 8);
impl crate::GuestArg for DXGI_RGB {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 3;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_RGBA {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}
const _: () = assert!(core::mem::size_of::<DXGI_RGBA>() == 16);
const _: () = assert!(core::mem::align_of::<DXGI_RGBA>() == 4);
impl Default for DXGI_RGBA { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_RGBA, r) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_RGBA, g) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_RGBA, b) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_RGBA, a) == 12);
impl crate::GuestArg for DXGI_RGBA {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 4;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SAMPLE_DESC {
    pub Count: u32,
    pub Quality: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_SAMPLE_DESC>() == 8);
const _: () = assert!(core::mem::align_of::<DXGI_SAMPLE_DESC>() == 4);
impl Default for DXGI_SAMPLE_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SAMPLE_DESC, Count) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_SAMPLE_DESC, Quality) == 4);
impl crate::GuestArg for DXGI_SAMPLE_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SHARED_RESOURCE {
    pub Handle: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_SHARED_RESOURCE>() == 4);
const _: () = assert!(core::mem::align_of::<DXGI_SHARED_RESOURCE>() == 4);
impl Default for DXGI_SHARED_RESOURCE { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SHARED_RESOURCE, Handle) == 0);
impl crate::GuestArg for DXGI_SHARED_RESOURCE {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SURFACE_DESC {
    pub Width: u32,
    pub Height: u32,
    pub Format: i32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
}
const _: () = assert!(core::mem::size_of::<DXGI_SURFACE_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<DXGI_SURFACE_DESC>() == 4);
impl Default for DXGI_SURFACE_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SURFACE_DESC, Width) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_SURFACE_DESC, Height) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_SURFACE_DESC, Format) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_SURFACE_DESC, SampleDesc) == 12);
impl crate::GuestArg for DXGI_SURFACE_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SWAP_CHAIN_DESC {
    pub BufferDesc: DXGI_MODE_DESC,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub BufferUsage: u32,
    pub BufferCount: u32,
    pub OutputWindow: u32,
    pub Windowed: i32,
    pub SwapEffect: i32,
    pub Flags: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_SWAP_CHAIN_DESC>() == 60);
const _: () = assert!(core::mem::align_of::<DXGI_SWAP_CHAIN_DESC>() == 4);
impl Default for DXGI_SWAP_CHAIN_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, BufferDesc) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, SampleDesc) == 28);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, BufferUsage) == 36);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, BufferCount) == 40);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, OutputWindow) == 44);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, Windowed) == 48);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, SwapEffect) == 52);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC, Flags) == 56);
impl crate::GuestArg for DXGI_SWAP_CHAIN_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 15;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SWAP_CHAIN_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub Format: i32,
    pub Stereo: i32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub BufferUsage: u32,
    pub BufferCount: u32,
    pub Scaling: i32,
    pub SwapEffect: i32,
    pub AlphaMode: i32,
    pub Flags: u32,
}
const _: () = assert!(core::mem::size_of::<DXGI_SWAP_CHAIN_DESC1>() == 48);
const _: () = assert!(core::mem::align_of::<DXGI_SWAP_CHAIN_DESC1>() == 4);
impl Default for DXGI_SWAP_CHAIN_DESC1 { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Width) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Height) == 4);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Format) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Stereo) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, SampleDesc) == 16);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, BufferUsage) == 24);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, BufferCount) == 28);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Scaling) == 32);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, SwapEffect) == 36);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, AlphaMode) == 40);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_DESC1, Flags) == 44);
impl crate::GuestArg for DXGI_SWAP_CHAIN_DESC1 {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 12;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
    pub RefreshRate: DXGI_RATIONAL,
    pub ScanlineOrdering: i32,
    pub Scaling: i32,
    pub Windowed: i32,
}
const _: () = assert!(core::mem::size_of::<DXGI_SWAP_CHAIN_FULLSCREEN_DESC>() == 20);
const _: () = assert!(core::mem::align_of::<DXGI_SWAP_CHAIN_FULLSCREEN_DESC>() == 4);
impl Default for DXGI_SWAP_CHAIN_FULLSCREEN_DESC { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_FULLSCREEN_DESC, RefreshRate) == 0);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_FULLSCREEN_DESC, ScanlineOrdering) == 8);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_FULLSCREEN_DESC, Scaling) == 12);
const _: () = assert!(core::mem::offset_of!(DXGI_SWAP_CHAIN_FULLSCREEN_DESC, Windowed) == 16);
impl crate::GuestArg for DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 5;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LUID {
    pub LowPart: u32,
    pub HighPart: i32,
}
const _: () = assert!(core::mem::size_of::<LUID>() == 8);
const _: () = assert!(core::mem::align_of::<LUID>() == 4);
impl Default for LUID { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }
const _: () = assert!(core::mem::offset_of!(LUID, LowPart) == 0);
const _: () = assert!(core::mem::offset_of!(LUID, HighPart) == 4);
impl crate::GuestArg for LUID {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DLINEPATTERN {
    pub wRepeatFactor: u16,
    pub wLinePattern: u16,
}
const _: () = assert!(core::mem::size_of::<D3DLINEPATTERN>() == 4);
const _: () = assert!(core::mem::align_of::<D3DLINEPATTERN>() == 2);
const _: () = assert!(core::mem::offset_of!(D3DLINEPATTERN, wRepeatFactor) == 0);
const _: () = assert!(core::mem::offset_of!(D3DLINEPATTERN, wLinePattern) == 2);
impl crate::GuestArg for D3DLINEPATTERN {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 1;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDEVINFO_TEXTUREMANAGER {
    pub bThrashing: i32,
    pub dwApproxBytesDownloaded: u32,
    pub dwNumEvicts: u32,
    pub dwNumVidCreates: u32,
    pub dwNumTexturesUsed: u32,
    pub dwNumUsedTexInVid: u32,
    pub dwWorkingSet: u32,
    pub dwWorkingSetBytes: u32,
    pub dwTotalManaged: u32,
    pub dwTotalBytes: u32,
    pub dwLastPri: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDEVINFO_TEXTUREMANAGER>() == 44);
const _: () = assert!(core::mem::align_of::<D3DDEVINFO_TEXTUREMANAGER>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, bThrashing) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwApproxBytesDownloaded) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwNumEvicts) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwNumVidCreates) == 12);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwNumTexturesUsed) == 16);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwNumUsedTexInVid) == 20);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwWorkingSet) == 24);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwWorkingSetBytes) == 28);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwTotalManaged) == 32);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwTotalBytes) == 36);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTUREMANAGER, dwLastPri) == 40);
impl crate::GuestArg for D3DDEVINFO_TEXTUREMANAGER {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 11;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct D3DDEVINFO_TEXTURING {
    pub dwNumLoads: u32,
    pub dwApproxBytesLoaded: u32,
    pub dwNumPreLoads: u32,
    pub dwNumSet: u32,
    pub dwNumCreates: u32,
    pub dwNumDestroys: u32,
    pub dwNumSetPriorities: u32,
    pub dwNumSetLODs: u32,
    pub dwNumLocks: u32,
    pub dwNumGetDCs: u32,
}
const _: () = assert!(core::mem::size_of::<D3DDEVINFO_TEXTURING>() == 40);
const _: () = assert!(core::mem::align_of::<D3DDEVINFO_TEXTURING>() == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumLoads) == 0);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwApproxBytesLoaded) == 4);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumPreLoads) == 8);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumSet) == 12);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumCreates) == 16);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumDestroys) == 20);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumSetPriorities) == 24);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumSetLODs) == 28);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumLocks) == 32);
const _: () = assert!(core::mem::offset_of!(D3DDEVINFO_TEXTURING, dwNumGetDCs) == 36);
impl crate::GuestArg for D3DDEVINFO_TEXTURING {
    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;
    const WORDS: u8 = 10;
    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }
}
