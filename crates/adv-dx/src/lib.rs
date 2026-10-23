//! COM device fronts: the interface semantics and object state of DirectX devices (locks,
//! reference counts, vtables in guest memory). Pixels, mixing and decoding belong to
//! adv-device. DirectDraw and D3D9 receive allocator, DC and window services from
//! assembly; other fronts use adv-os process state. This crate never depends on adv-win32.

#[cfg(feature = "gpu")]
pub mod d3d9;
pub mod d3dx9;
pub mod ddraw;
pub mod dinput;
pub mod dshow;
pub mod dsound;
pub mod dxdiag;
pub mod shader;
pub mod wmvcore;

pub mod interop;
mod refcounts;
