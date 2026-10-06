//! COM device fronts: the interface semantics and object state of DirectX devices (locks,
//! reference counts, vtables in guest memory). Pixels, mixing and decoding belong to
//! adv-device; shared process state to adv-os. This crate never depends on adv-win32.

pub mod ddraw;
pub mod dsound;
pub mod dinput;
pub mod dshow;
#[cfg(feature = "gpu")]
pub mod d3d9;
