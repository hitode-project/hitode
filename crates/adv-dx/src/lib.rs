//! COM device fronts: the interface semantics and object state of DirectX devices (locks,
//! reference counts, vtables in guest memory). Pixels, mixing and decoding belong to
//! adv-device. DirectDraw and D3D9 receive allocator, DC and window services from
//! assembly; other fronts use adv-os process state.

#[cfg(feature = "gpu")]
pub mod d3d9;
pub mod ddraw;
pub mod dshow;
pub mod dsound;
pub mod shader;
pub mod wmvcore;
pub mod xaudio2;

pub mod interop;
mod refcounts;

#[cfg(test)]
extern crate self as adv_dx;
#[cfg(test)]
mod test_support;
