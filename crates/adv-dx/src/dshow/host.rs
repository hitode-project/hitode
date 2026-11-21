//! Assembly seams for Wine's files, notifications and guest allocation.
use super::*;

/// A source opened by file name, with the handle that keeps its sharing lock,
/// and its decoder unless the contents do not decode. Contents read out
/// through the guest come without a handle: theirs was closed as it opened.
pub type Opened = Box<
    dyn FnOnce(
        &mut Env<'_>,
        Result<(Option<adv_device::media::Decoder>, Option<u32>), u32>,
    ) -> Outcome,
>;
pub type DemuxFile =
    Box<dyn FnOnce(&mut Env<'_>, Result<adv_device::media::Demuxed, u32>) -> Outcome>;
pub type ReadFile = Box<dyn FnOnce(&mut Env<'_>, Result<(Vec<u8>, Option<u32>), u32>) -> Outcome>;
/// Byte ranges of a file: `(offset, length)`.
pub type Ranges = Vec<(u64, usize)>;
/// The bytes of each requested range, `None` where the file is shorter.
pub type Sampled = Box<dyn FnOnce(&mut Env<'_>, Result<Vec<Option<Vec<u8>>>, u32>) -> Outcome>;

/// One registered media type, with its source filter and byte patterns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaEntry {
    pub major: Option<adv_os::com::Iid>,
    pub sub: Option<adv_os::com::Iid>,
    pub source: Option<adv_os::com::Iid>,
    /// Pattern strings: "offset, size, mask, value" tuples, all of which match.
    pub patterns: Vec<String>,
}

/// The registry's answer for one file.
#[derive(Debug, Default)]
pub struct MediaTypes {
    /// `Media Type\Extensions\<extension>`, when registered.
    pub extension: Option<MediaEntry>,
    /// Every `Media Type\{major}\{subtype}` entry, read only without one.
    pub entries: Vec<MediaEntry>,
}

/// The file Wine's own quartz keeps once the native front takes `quartz.dll`.
pub const WINE_QUARTZ: &str = "winequartz.dll";

#[derive(Clone, Copy)]
pub struct Host {
    pub allocator: crate::interop::GuestAllocator,
    pub open: fn(&mut Env<'_>, u32, Opened) -> Outcome,
    pub demux: fn(&mut Env<'_>, u32, DemuxFile) -> Outcome,
    pub read: fn(&mut Env<'_>, u32, ReadFile) -> Outcome,
    pub close: fn(&mut Env<'_>, u32),
    pub thread: fn(&mut Env<'_>, u32) -> Outcome,
    pub event: fn(&mut Env<'_>) -> Result<u32, String>,
    pub signal: fn(&mut Env<'_>, u32, bool),
    pub post: fn(&mut Env<'_>, [u32; 4]),
    /// `HKCR\Media Type` for a file with this extension, dot included.
    pub media_types: fn(&mut Env<'_>, Option<&str>) -> MediaTypes,
    /// Read each `(offset, length)` range of the named file through the
    /// guest, then close it.
    pub sample: fn(&mut Env<'_>, u32, Ranges, Sampled) -> Outcome,
    /// The address of a loaded module's export; Wine's own quartz is
    /// [`WINE_QUARTZ`].
    pub export: fn(&Env<'_>, &str, &str) -> Option<u32>,
}

pub(crate) fn close(env: &mut Env<'_>, handle: u32) {
    if let Some(host) = env.ext.try_get::<Host>() {
        (host.close)(env, handle);
    } else {
        env.sys.objects.close(handle);
    }
}

const IID_ICLASSFACTORY: adv_os::com::Iid = super::guid(
    0x0000_0001,
    0x0000,
    0x0000,
    [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);

/// Create `class` from Wine's own quartz, as CoCreateInstance would, and hand
/// the new `iid` interface (zero on failure) to `then` with the HRESULT.
pub(crate) fn create_wine(
    env: &mut Env<'_>,
    class: adv_os::com::Iid,
    outer: u32,
    iid: adv_os::com::Iid,
    then: impl FnOnce(&mut Env<'_>, u32, u32) -> Outcome + 'static,
) -> Outcome {
    use super::{E_NOINTERFACE, E_OUTOFMEMORY, S_OK, alloc, free, guest_call};
    use adv_cpu::backend::GuestExt;
    let entry = env
        .ext
        .try_get::<Host>()
        .and_then(|host| (host.export)(env, WINE_QUARTZ, "DllGetClassObject"));
    let Some(entry) = entry else {
        return then(env, 0, E_NOINTERFACE);
    };
    let Some(scratch) = alloc(env, 40) else {
        return then(env, 0, E_OUTOFMEMORY);
    };
    let written = env
        .cpu
        .write(scratch, &class)
        .and_then(|()| env.cpu.write(scratch + 16, &IID_ICLASSFACTORY));
    if written.is_err() {
        free(env, scratch);
        return then(env, 0, E_OUTOFMEMORY);
    }
    let args = vec![scratch, scratch + 16, scratch + 32];
    Outcome::call(entry, args, move |env, resume| {
        let factory = env.cpu.read_u32(scratch + 32).unwrap_or(0);
        let hr = resume.value();
        if hr != S_OK || factory == 0 || env.cpu.write(scratch + 16, &iid).is_err() {
            free(env, scratch);
            return then(env, 0, if hr == S_OK { E_NOINTERFACE } else { hr });
        }
        let args = vec![outer, scratch + 16, scratch + 36];
        guest_call(env, factory, 3, args, move |env, hr| {
            let object = match hr {
                S_OK => env.cpu.read_u32(scratch + 36).unwrap_or(0),
                _ => 0,
            };
            guest_call(env, factory, 2, vec![], move |env, _| {
                free(env, scratch);
                then(
                    env,
                    object,
                    if object == 0 && hr == S_OK {
                        E_NOINTERFACE
                    } else {
                        hr
                    },
                )
            })
        })
    })
}
