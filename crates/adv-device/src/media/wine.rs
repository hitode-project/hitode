//! Asynchronous container and sample decoding, with caller-supplied input bytes.
use super::{CodecFormat, Error};

#[cfg(feature = "media")]
mod decode;
#[cfg(feature = "media")]
pub use decode::{Sample, Worker};

#[derive(Clone, Debug)]
pub struct Track {
    pub codec: CodecFormat,
    pub language: Option<String>,
    pub name: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Info {
    pub tracks: Vec<Track>,
    pub duration: u64,
    pub mime: String,
}
pub enum Event {
    Read { offset: u64, size: u32 },
    Ready(Result<Info, Error>),
    Sample(usize, Result<Option<Sample>, Error>),
    Done(Result<(), Error>),
}
pub enum Command {
    Next(usize, bool),
    Seek(u64),
    Decode(Vec<u8>, Option<i64>, i64),
    Drain,
    Flush,
}
#[derive(Clone, Copy, Debug)]
pub enum Output {
    Video {
        pixel: u32,
        width: u32,
        height: i32,
        align: u32,
    },
    Audio {
        sample: u32,
        channels: u32,
        rate: u32,
    },
    Compressed,
}

#[cfg(not(feature = "media"))]
pub struct Worker;
#[cfg(not(feature = "media"))]
pub struct Sample {
    pub pts: i64,
    pub duration: i64,
    pub key: bool,
}
#[cfg(not(feature = "media"))]
impl Worker {
    pub fn parser(_: u64, _: adv_sync::WakeEvent) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }
    pub fn transform(_: CodecFormat, _: adv_sync::WakeEvent) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }
    pub fn send(&self, _: Command) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    pub fn push(&self, _: Vec<u8>) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    pub fn poll(&self) -> Option<Event> {
        None
    }
}
#[cfg(not(feature = "media"))]
impl Sample {
    pub fn size(&self, _: Output) -> Result<usize, Error> {
        Err(Error::Unavailable)
    }
    pub fn copy(&self, _: Output, _: &mut [u8]) -> Result<usize, Error> {
        Err(Error::Unavailable)
    }
}
