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
