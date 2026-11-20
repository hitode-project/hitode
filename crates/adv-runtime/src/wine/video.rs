//! Native media fronts, with Wine kept under separate names for other exports
//! and classes. Files still open through Wine and retain its sharing lock.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, Weak};

use adv_abi::dll::{Export, Import, ModuleImage};
use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt};
use adv_device::media::{Decoder, ReadSeek};
use adv_device::surface::SurfaceStore;
use adv_machine::{Env, Machine, Outcome, Wait};
use adv_nt::process::{module_base, module_export};
use adv_pe::{ImportModule, ImportName};

const QUARTZ: &str = "quartz.dll";
const WMVCORE: &str = "wmvcore.dll";
const QASF: &str = "qasf.dll";
const READ_THREAD: &str = "<DirectShowReadThread>";
const E_FAIL: u32 = 0x8000_4005;
const ORIGINAL_QUARTZ: &str = adv_dx::dshow::WINE_QUARTZ;
const ORIGINAL_WMVCORE: &str = "winewmvcore.dll";
const ORIGINAL_QASF: &str = "wineqasf.dll";
// Wine 11.0's spec files, including the explicitly assigned wmvcore ordinals.
type ExportSpec = (&'static str, u16, u8);
const QUARTZ_EXPORTS: &[ExportSpec] = &[
    ("AMGetErrorTextA", 1, 3),
    ("AMGetErrorTextW", 2, 3),
    ("AmpFactorToDB", 3, 1),
    ("DBToAmpFactor", 4, 1),
    ("DllCanUnloadNow", 5, 0),
    ("DllGetClassObject", 6, 3),
    ("DllRegisterServer", 7, 0),
    ("DllUnregisterServer", 8, 0),
    ("GetProxyDllInfo", 9, 0),
];
const WMVCORE_EXPORTS: &[ExportSpec] = &[
    ("WMCheckURLExtension", 1, 1),
    ("WMCheckURLScheme", 2, 1),
    ("WMCreateBackupRestorerPrivate", 3, 2),
    ("WMIsAvailableOffline", 4, 0),
    ("WMValidateData", 5, 0),
    ("DllRegisterServer", 6, 0),
    ("WMCreateBackupRestorer", 7, 2),
    ("WMCreateEditor", 8, 1),
    ("WMCreateIndexer", 9, 0),
    ("WMCreateProfileManager", 10, 1),
    ("WMCreateReader", 11, 3),
    ("WMCreateReaderPriv", 12, 1),
    ("WMCreateSyncReader", 13, 3),
    ("WMCreateSyncReaderPriv", 14, 1),
    ("WMCreateWriter", 15, 2),
    ("WMCreateWriterFileSink", 16, 0),
    ("WMCreateWriterNetworkSink", 17, 0),
    ("WMCreateWriterPriv", 18, 1),
    ("WMCreateWriterPushSink", 19, 0),
    ("WMIsContentProtected", 20, 2),
];

const QASF_EXPORTS: &[ExportSpec] = &[
    ("DllCanUnloadNow", 1, 0),
    ("DllGetClassObject", 2, 3),
    ("DllRegisterServer", 3, 0),
    ("DllUnregisterServer", 4, 0),
];
const MODULES: &[(&str, &str, &[ExportSpec])] = &[
    (QUARTZ, ORIGINAL_QUARTZ, QUARTZ_EXPORTS),
    (WMVCORE, ORIGINAL_WMVCORE, WMVCORE_EXPORTS),
    (QASF, ORIGINAL_QASF, QASF_EXPORTS),
];

pub(super) fn register(registry: &mut Registry) {
    adv_dx::dshow::register(registry);
    adv_dx::wmvcore::register(registry);
    registry.add(QUARTZ, READ_THREAD, Conv::Stdcall { argc: 1 }, read_thread);
    for (module_index, &(module, _, exports)) in MODULES.iter().enumerate() {
        for (index, &(name, _, argc)) in exports.iter().enumerate() {
            if module == WMVCORE && name == "WMCreateSyncReader" {
                continue;
            }
            registry.add_with_data(
                module,
                name,
                Conv::Stdcall { argc },
                forward,
                ((module_index as u64) << 32) | index as u64,
            );
        }
    }
}

fn forward(env: &mut Env<'_>) -> Outcome {
    let data = env.sys.thunks.get(env.thunk).f.data;
    let (module, original, exports) = MODULES[(data >> 32) as usize];
    let (name, _, argc) = exports[data as u32 as usize];
    if matches!(module, QUARTZ | QASF) && name == "DllCanUnloadNow" {
        return Outcome::Return(1);
    }
    if matches!(module, QUARTZ | QASF) && name == "DllGetClassObject" {
        let mut class = [0; 16];
        if env.cpu.read(env.arg(0), &mut class).is_ok() && adv_dx::dshow::has_graph_class(&class) {
            return super::class::get_class_object(env, |class| {
                Some(super::class::Activation::Class(class))
            });
        }
    }
    let Some(import) = Import::bind(env, original, name) else {
        return Outcome::Fail(format!("{module}: missing import {name}"));
    };
    let args: Vec<_> = (0..u32::from(argc)).map(|i| env.arg(i)).collect();
    import.call(env, args, |_, result| Outcome::Return(result.value()))
}

pub(super) fn install<B: Backend>(machine: &mut Machine<B>, drive: &Path) -> Result<(), String> {
    adv_dx::dshow::install(machine);
    machine.ext.get_mut::<adv_os::Heaps>().process_heap = machine.ext.get::<super::Heap>().0;
    machine.ext.insert(adv_dx::dshow::Host {
        allocator: super::allocator(),
        open,
        demux: demux_file,
        read: read_file,
        close,
        thread,
        post,
        export: |env, module, name| export(env, module, name).ok(),
        media_types,
        sample,
        event: |env| {
            let mut request = adv_nt::protocol::CreateEventRequest {
                access: 0x1f0003,
                manual_reset: 1,
                ..Default::default()
            };
            let (reply, _) = adv_nt::server::request(env, &mut request, &[], &mut [])
                .map_err(|e| format!("{e:?}"))?;
            adv_nt::status::check(reply.header.error).map_err(|e| format!("{e:?}"))?;
            Ok(reply.handle)
        },
        signal: |env, handle, set| {
            if set {
                let _ = adv_nt::sync::set_audio_event(env, handle);
            } else {
                let _ = adv_nt::sync::reset_event(env, &[handle, 0]);
            }
        },
    });
    adv_dx::dshow::set_video_provider(
        &mut machine.ext,
        adv_dx::dshow::VideoProvider {
            window: |ext, _, hwnd| {
                let window = adv_drv::user::window(ext, hwnd)?;
                let (origin, visible) = adv_drv::user::client_geometry(ext, hwnd)?;
                let canvas_hwnd = adv_drv::user::input::presented_window(ext).unwrap_or(hwnd);
                let canvas_origin = adv_drv::user::client_geometry(ext, canvas_hwnd)?.0;
                let canvas = adv_drv::user::input::presented_window(ext)
                    .and_then(|hwnd| adv_drv::user::window(ext, hwnd))
                    .unwrap_or(window);
                let [left, top, right, bottom] = window.client;
                let [x, y, r, b] = canvas.client;
                Some(adv_dx::dshow::VideoWindowGeometry {
                    width: (right - left) as u32,
                    height: (bottom - top) as u32,
                    origin: (origin.0 - canvas_origin.0, origin.1 - canvas_origin.1),
                    canvas: ((r - x) as u32, (b - y) as u32),
                    visible,
                })
            },
            update: |ext, cpu, key, layer| {
                ext.get_mut::<SurfaceStore>()
                    .set_video_layer(key, layer, &super::guest_memory(cpu))
            },
            completed: super::frame_completed,
        },
    );
    for &(module, original, exports) in MODULES {
        let directory = drive.join("windows/system32");
        let retained = directory.join(original);
        if !retained.exists() {
            std::fs::copy(directory.join(module), &retained).map_err(|e| e.to_string())?;
        }
        let imports = [ImportModule {
            dll: original.into(),
            imports: exports
                .iter()
                .filter(|&&(name, _, _)| module != WMVCORE || name != "WMCreateSyncReader")
                .map(|&(name, _, _)| adv_pe::Import {
                    name: ImportName::Name {
                        hint: 0,
                        name: name.into(),
                    },
                    iat_rva: 0,
                })
                .collect(),
        }];
        let exports: Vec<_> = exports
            .iter()
            .map(|&(name, ordinal, _)| Export { name, ordinal })
            .collect();
        let wine = std::fs::read(&retained).map_err(|e| e.to_string())?;
        let image = ModuleImage::build_replacing(machine, module, &exports, &imports, &wine)?;
        adv_unix::builtin::install(drive, &image).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn export(env: &Env<'_>, module: &str, name: &str) -> Result<u32, String> {
    // kernel32 forwards these file and thread exports to kernelbase in Wine.
    [module, "kernelbase.dll"]
        .into_iter()
        .find_map(|module| {
            module_base(env.cpu, env.sys.peb, module)
                .and_then(|base| module_export(env.cpu, base, name))
        })
        .ok_or_else(|| format!("{module}: missing {name}"))
}

fn close(env: &mut Env<'_>, handle: u32) {
    let _ = adv_nt::server::close(env, handle);
}
fn release(env: &mut Env<'_>, handle: Option<u32>) {
    if let Some(handle) = handle {
        close(env, handle);
    }
}

type Contents = Box<dyn ReadSeek>;
type FileOpened = Box<dyn FnOnce(&mut Env<'_>, Result<(Contents, Option<u32>), u32>) -> Outcome>;
/// Opens `filename` as the guest does. Its contents come from the host
/// descriptor, unless guest code hooked the file read path: a file
/// virtualizer answers reads of a placeholder handle itself, so the
/// contents are then read through the guest's ReadFile, and the handle is
/// closed through the guest too, so the hooks that saw it open see it close.
fn file(env: &mut Env<'_>, filename: u32, then: FileOpened) -> Outcome {
    let target = match export(env, "kernel32.dll", "CreateFileW") {
        Ok(target) => target,
        Err(error) => return Outcome::Fail(error),
    };
    Outcome::call(
        target,
        [filename, 0x8000_0000, 1, 0, 3, 0x80, 0],
        move |env, result| {
            let handle = result.value();
            if handle == u32::MAX {
                // HRESULT_FROM_WIN32 of the thread's last error, as quartz returns.
                let error = env.cpu.read_u32(env.ctx().teb + 0x34).unwrap_or(0);
                let hr = if error == 0 {
                    0x8000_4005
                } else {
                    0x8007_0000 | (error & 0xffff)
                };
                return then(env, Err(hr));
            }
            if adv_nt::identity::reads_hooked(env.cpu, env.ext) {
                return guest_contents(env, handle, then);
            }
            let result = adv_nt::server::handle_fd(env, handle)
                .map_err(|_| 0x8000_4005u32)
                .and_then(|fd| {
                    // SAFETY: the server owns this fd; duplication transfers only the clone.
                    let fd = unsafe { BorrowedFd::borrow_raw(fd.fd) }
                        .try_clone_to_owned()
                        .map_err(|_| 0x8000_4005u32)?;
                    Ok((Box::new(File::from(fd)) as Contents, Some(handle)))
                });
            if result.is_err() {
                close(env, handle);
            }
            then(env, result)
        },
    )
}

/// Keep the guest's sequential ReadFile contract while letting AVIO seek
/// within bytes already read. Reads ahead are driven by AVIO, not by file size.
const CHUNK: u32 = 256 << 10;

#[derive(Default)]
struct Progress {
    written: u64,
    requested: u64,
    done: Option<bool>,
    closed: bool,
}

struct SharedRead {
    progress: Mutex<Progress>,
    changed: Condvar,
    wake: Option<adv_sync::WakeEvent>,
}

impl SharedRead {
    fn signal(&self) {
        if let Some(wake) = &self.wake {
            wake.signal();
        }
    }
}

struct GuestReader {
    file: File,
    shared: Arc<SharedRead>,
}

impl GuestReader {
    fn require(&self, offset: u64, end: u64) -> io::Result<()> {
        let mut p = self.shared.progress.lock().unwrap();
        if p.written <= offset && p.done.is_none() {
            p.requested = p.requested.max(end);
            self.shared.signal();
            p = self
                .shared
                .changed
                .wait_while(p, |p| p.written <= offset && p.done.is_none())
                .unwrap();
        }
        if p.done == Some(false) {
            return Err(io::Error::other("guest ReadFile failed"));
        }
        Ok(())
    }
}

impl Read for GuestReader {
    fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
        if dest.is_empty() {
            return Ok(0);
        }
        let offset = self.file.stream_position()?;
        self.require(offset, offset.saturating_add(dest.len() as u64))?;
        self.file.read(dest)
    }
}

impl Seek for GuestReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if matches!(from, SeekFrom::End(_)) {
            self.require(u64::MAX, u64::MAX)?;
        }
        self.file.seek(from)
    }
}

impl Drop for GuestReader {
    fn drop(&mut self) {
        self.shared.progress.lock().unwrap().closed = true;
        self.shared.signal();
    }
}

struct GuestWriter {
    file: File,
    shared: Arc<SharedRead>,
    handle: u32,
    read: u32,
    wake: adv_machine::Handle,
}

impl Drop for GuestWriter {
    fn drop(&mut self) {
        let mut progress = self.shared.progress.lock().unwrap();
        progress.done.get_or_insert(false);
        self.shared.changed.notify_all();
    }
}

#[derive(Default)]
struct GuestReads {
    pending: HashMap<adv_machine::Handle, GuestWriter>,
    active: HashMap<adv_machine::Handle, Weak<SharedRead>>,
}

fn buffer_file() -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    loop {
        let next = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = dir.join(format!("hitode-media-{}-{next}", std::process::id()));
        match File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&name)
        {
            Ok(file) => {
                fs::remove_file(name)?;
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

fn guest_contents(env: &mut Env<'_>, handle: u32, then: FileOpened) -> Outcome {
    let (Ok(read), Ok(file)) = (export(env, "kernel32.dll", "ReadFile"), buffer_file()) else {
        return guest_close(env, handle, move |env| then(env, Err(E_FAIL)));
    };
    let Ok(output) = file.try_clone() else {
        return guest_close(env, handle, move |env| then(env, Err(E_FAIL)));
    };
    let shared = Arc::new(SharedRead {
        progress: Mutex::new(Progress::default()),
        changed: Condvar::new(),
        wake: env.sys.poll_wake.clone(),
    });
    let wake = env.sys.objects.insert(adv_machine::Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let reads = env.ext.get_or_default::<GuestReads>();
    reads.active.insert(wake, Arc::downgrade(&shared));
    reads.pending.insert(
        wake,
        GuestWriter {
            file: output,
            shared: shared.clone(),
            handle,
            read,
            wake,
        },
    );
    guest_thread(env, READ_THREAD, wake, move |env, hr| {
        if hr != 0 {
            let reads = env.ext.get_mut::<GuestReads>();
            reads.active.remove(&wake);
            reads.pending.remove(&wake);
            env.sys.objects.close(wake);
            return guest_close(env, handle, move |env| then(env, Err(hr)));
        }
        then(env, Ok((Box::new(GuestReader { file, shared }), None)))
    })
}

fn read_thread(env: &mut Env<'_>) -> Outcome {
    let wake = env.arg(0);
    let Some(writer) = env.ext.get_or_default::<GuestReads>().pending.remove(&wake) else {
        return Outcome::Return(E_FAIL);
    };
    let Some(buffer) = (super::allocator().allocate)(env, CHUNK + 4) else {
        return close_writer(env, writer, false);
    };
    copy_requested(env, writer, buffer, vec![0; CHUNK as usize])
}

/// Deliver a host reader's request onto the guest copy thread.
#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub(super) fn pump_reads<B: Backend>(machine: &mut Machine<B>) {
    if let Some(reads) = machine.ext.try_get::<GuestReads>() {
        for (&wake, shared) in &reads.active {
            if let Some(shared) = shared.upgrade() {
                let p = shared.progress.lock().unwrap();
                if p.closed || p.requested > p.written {
                    machine.sys.set_event(wake);
                }
            }
        }
    }
}

fn close_writer(env: &mut Env<'_>, writer: GuestWriter, success: bool) -> Outcome {
    {
        let mut p = writer.shared.progress.lock().unwrap();
        p.done = Some(success);
        writer.shared.changed.notify_all();
    }
    env.ext.get_mut::<GuestReads>().active.remove(&writer.wake);
    env.sys.objects.close(writer.wake);
    guest_close(env, writer.handle, |_| Outcome::Return(0))
}

fn copy_requested(
    env: &mut Env<'_>,
    writer: GuestWriter,
    buffer: u32,
    mut scratch: Vec<u8>,
) -> Outcome {
    let (offset, requested, closed) = {
        let p = writer.shared.progress.lock().unwrap();
        (p.written, p.requested, p.closed)
    };
    if closed {
        (super::allocator().free)(env, buffer);
        return close_writer(env, writer, true);
    }
    if offset >= requested {
        let wake = writer.wake;
        let deadline = writer
            .shared
            .wake
            .is_none()
            .then(|| env.sys.clock.now_us() + 1000);
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline,
            },
            move |env, _| copy_requested(env, writer, buffer, scratch),
        );
    }
    let args = [writer.handle, buffer, CHUNK, buffer + CHUNK, 0];
    Outcome::call(writer.read, args, move |env, result| {
        let count = env.cpu.read_u32(buffer + CHUNK).unwrap_or(0) as usize;
        let success = result.value() != 0
            && count <= scratch.len()
            && env.cpu.read(buffer, &mut scratch[..count]).is_ok()
            && writer.file.write_all_at(&scratch[..count], offset).is_ok();
        if !success || count == 0 {
            (super::allocator().free)(env, buffer);
            return close_writer(env, writer, success);
        }
        {
            let mut p = writer.shared.progress.lock().unwrap();
            p.written += count as u64;
            writer.shared.changed.notify_all();
        }
        copy_requested(env, writer, buffer, scratch)
    })
}

fn guest_close(
    env: &mut Env<'_>,
    handle: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    match export(env, "kernel32.dll", "CloseHandle") {
        Ok(target) => Outcome::call(target, [handle], move |env, _| then(env)),
        Err(error) => Outcome::Fail(error),
    }
}

fn open(env: &mut Env<'_>, filename: u32, then: adv_dx::dshow::Opened) -> Outcome {
    read_on_worker(
        env,
        filename,
        |file| Ok(Decoder::open_reader(file).ok()),
        then,
    )
}

type ReadDone<T> = Box<dyn FnOnce(&mut Env<'_>, Result<(T, Option<u32>), u32>) -> Outcome>;

fn read_on_worker<T: Send + 'static>(
    env: &mut Env<'_>,
    filename: u32,
    work: impl FnOnce(Contents) -> Result<T, u32> + Send + 'static,
    then: ReadDone<T>,
) -> Outcome {
    file(
        env,
        filename,
        Box::new(move |env, result| {
            let (file, handle) = match result {
                Ok(value) => value,
                Err(hr) => return then(env, Err(hr)),
            };
            let (send, receive) = std::sync::mpsc::channel();
            if std::thread::Builder::new()
                .name("media-open".into())
                .spawn(move || {
                    let _ = send.send(work(file));
                })
                .is_err()
            {
                release(env, handle);
                return then(env, Err(E_FAIL));
            }
            read_result(env, handle, receive, then)
        }),
    )
}

fn read_result<T: 'static>(
    env: &mut Env<'_>,
    handle: Option<u32>,
    receive: Receiver<Result<T, u32>>,
    then: ReadDone<T>,
) -> Outcome {
    let error = match receive.try_recv() {
        Ok(Ok(value)) => return then(env, Ok((value, handle))),
        Ok(Err(hr)) => hr,
        Err(TryRecvError::Empty) => {
            return Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                read_result(env, handle, receive, then)
            });
        }
        Err(TryRecvError::Disconnected) => E_FAIL,
    };
    release(env, handle);
    then(env, Err(error))
}
const MEDIA_TYPES: &str = "\\Registry\\Machine\\Software\\Classes\\Media Type";
/// Registry strings longer than this are no GUID or pattern quartz reads.
const MEDIA_VALUE: usize = 4096;

fn registry_text(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn registry_guid(env: &mut Env<'_>, key: u32, name: &str) -> Option<adv_os::com::Iid> {
    let (_, data) = adv_nt::registry::value(env, key, name, MEDIA_VALUE).ok()??;
    adv_os::com::parse_guid(&registry_text(&data))
}

fn subkeys(env: &mut Env<'_>, key: u32) -> Vec<String> {
    (0..)
        .map_while(|index| adv_nt::registry::subkey(env, key, index).ok().flatten())
        .map(|name| String::from_utf16_lossy(&name))
        .collect()
}

/// `HKCR\Media Type`, read as quartz's async reader reads it.
fn media_types(env: &mut Env<'_>, extension: Option<&str>) -> adv_dx::dshow::MediaTypes {
    use adv_dx::dshow::{MediaEntry, MediaTypes};
    let mut types = MediaTypes::default();
    let Ok(root) = adv_nt::registry::open_for_reading(env, 0, MEDIA_TYPES) else {
        return types;
    };
    let path = extension.map(|extension| format!("Extensions\\{extension}"));
    if let Some(key) =
        path.and_then(|path| adv_nt::registry::open_for_reading(env, root, &path).ok())
    {
        types.extension = Some(MediaEntry {
            major: registry_guid(env, key, "Media Type"),
            sub: registry_guid(env, key, "Subtype"),
            source: registry_guid(env, key, "Source Filter"),
            patterns: Vec::new(),
        });
        let _ = adv_nt::server::close(env, key);
        let _ = adv_nt::server::close(env, root);
        return types;
    }
    for major_name in subkeys(env, root) {
        if major_name == "Extensions" {
            continue;
        }
        let Ok(major) = adv_nt::registry::open_for_reading(env, root, &major_name) else {
            continue;
        };
        for sub_name in subkeys(env, major) {
            let Ok(sub) = adv_nt::registry::open_for_reading(env, major, &sub_name) else {
                continue;
            };
            let patterns = (0..)
                .map_while(|index| {
                    adv_nt::registry::value_at(env, sub, index, MEDIA_VALUE)
                        .ok()
                        .flatten()
                })
                .filter(|value| String::from_utf16_lossy(&value.name) != "Source Filter")
                .map(|value| registry_text(&value.data))
                .collect();
            types.entries.push(MediaEntry {
                major: adv_os::com::parse_guid(&major_name),
                sub: adv_os::com::parse_guid(&sub_name),
                source: registry_guid(env, sub, "Source Filter"),
                patterns,
            });
            let _ = adv_nt::server::close(env, sub);
        }
        let _ = adv_nt::server::close(env, major);
    }
    let _ = adv_nt::server::close(env, root);
    types
}

/// The requested byte ranges of a file opened through the guest.
fn sample(
    env: &mut Env<'_>,
    filename: u32,
    ranges: adv_dx::dshow::Ranges,
    then: adv_dx::dshow::Sampled,
) -> Outcome {
    read_on_worker(
        env,
        filename,
        move |mut file| {
            Ok(ranges
                .into_iter()
                .map(|(offset, length)| {
                    let mut bytes = vec![0; length];
                    file.seek(SeekFrom::Start(offset)).ok()?;
                    file.read_exact(&mut bytes).ok()?;
                    Some(bytes)
                })
                .collect())
        },
        Box::new(move |env, result| then_without_handle(env, result, then)),
    )
}

fn demux_file(env: &mut Env<'_>, filename: u32, then: adv_dx::dshow::DemuxFile) -> Outcome {
    read_on_worker(
        env,
        filename,
        |file| {
            adv_device::media::demux_reader(file).map_err(|error| {
                log::warn!("DirectShow: packet demux failed: {error}");
                E_FAIL
            })
        },
        Box::new(move |env, result| then_without_handle(env, result, then)),
    )
}

fn read_file(env: &mut Env<'_>, filename: u32, then: adv_dx::dshow::ReadFile) -> Outcome {
    read_on_worker(
        env,
        filename,
        |mut file| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|_| E_FAIL)?;
            Ok(bytes)
        },
        then,
    )
}

fn then_without_handle<T>(
    env: &mut Env<'_>,
    result: Result<(T, Option<u32>), u32>,
    then: impl FnOnce(&mut Env<'_>, Result<T, u32>) -> Outcome,
) -> Outcome {
    match result {
        Ok((value, handle)) => {
            release(env, handle);
            then(env, Ok(value))
        }
        Err(hr) => then(env, Err(hr)),
    }
}

fn thread(env: &mut Env<'_>, id: u32) -> Outcome {
    guest_thread(env, "<DirectShowStreamThread>", id, |_, hr| {
        Outcome::Return(hr)
    })
}

fn guest_thread(
    env: &mut Env<'_>,
    name: &'static str,
    id: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let Ok(create) = export(env, "kernel32.dll", "CreateThread") else {
        return then(env, E_FAIL);
    };
    let Ok(entry) = env.sys.thunks.publish(env.cpu, QUARTZ, name) else {
        return then(env, E_FAIL);
    };
    Outcome::call(create, [0, 0, entry, id, 0, 0], move |env, result| {
        let handle = result.value();
        if handle == 0 {
            let error = env.cpu.read_u32(env.ctx().teb + 0x34).unwrap_or(0);
            return then(
                env,
                if error == 0 {
                    E_FAIL
                } else {
                    0x8007_0000 | (error & 0xffff)
                },
            );
        }
        close(env, handle);
        then(env, 0)
    })
}
fn post(env: &mut Env<'_>, args: [u32; 4]) {
    adv_drv::user::post_message(env.ext, args);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // spec: MEDIA-GRAPH-8
    #[test]
    fn guest_reader_returns_the_prefix_before_eof_and_can_seek_back() {
        let file = buffer_file().unwrap();
        let writer = file.try_clone().unwrap();
        let wake = adv_sync::WakeEvent::new().unwrap();
        let shared = Arc::new(SharedRead {
            progress: Mutex::new(Progress::default()),
            changed: Condvar::new(),
            wake: Some(wake.clone()),
        });
        let mut reader = GuestReader {
            file,
            shared: shared.clone(),
        };
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut bytes = [0; 4];
            reader.read_exact(&mut bytes).unwrap();
            reader.rewind().unwrap();
            let mut prefix = [0; 2];
            reader.read_exact(&mut prefix).unwrap();
            send.send((bytes, prefix)).unwrap();
        });
        let timeout = Duration::from_secs(3);
        assert!(wake.wait(Some(timeout)).unwrap());
        {
            let mut progress = shared.progress.lock().unwrap();
            assert_eq!(progress.requested, 4);
            assert!(progress.done.is_none());
            writer.write_all_at(b"head", 0).unwrap();
            progress.written = 4;
            shared.changed.notify_all();
        }
        assert_eq!(receive.recv_timeout(timeout).unwrap(), (*b"head", *b"he"));
        thread.join().unwrap();
        let progress = shared.progress.lock().unwrap();
        assert!(progress.closed);
        assert!(progress.done.is_none());
    }
}
