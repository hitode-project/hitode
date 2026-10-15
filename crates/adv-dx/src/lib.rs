//! COM device fronts: the interface semantics and object state of DirectX devices (locks,
//! reference counts, vtables in guest memory). Pixels, mixing and decoding belong to
//! adv-device; shared process state to adv-os. This crate never depends on adv-win32.

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

mod refcounts;
