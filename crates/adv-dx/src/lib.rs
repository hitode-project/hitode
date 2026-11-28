//! COM device fronts: the interface semantics and object state of DirectX devices (locks,
//! reference counts, vtables in guest memory). Pixels, mixing and decoding belong to
//! adv-device. DirectDraw and D3D9 receive allocator, DC and window services from
//! assembly; other fronts use adv-os process state.

pub mod d3d11;
#[cfg(feature = "gpu")]
pub mod d3d9;
pub mod ddraw;
pub mod dmsynth;
pub mod dshow;
pub mod dsound;
pub mod dxgi;
#[cfg(feature = "gpu")]
pub(crate) mod fixed;
pub(crate) mod pacing;
#[cfg(feature = "gpu")]
pub(crate) mod record;
pub mod shader;
#[cfg(feature = "gpu")]
pub(crate) mod text;
pub mod wmvcore;
pub mod xaudio2;

pub mod argument;
pub mod interop;
mod refcounts;

#[cfg(test)]
extern crate self as adv_dx;
#[cfg(test)]
mod test_support;
