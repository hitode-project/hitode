//! Native media fronts, with Wine kept under separate names for other exports
//! and classes. Files still open through Wine and retain its sharing lock.
use std::fs::File;
use std::io::{Cursor, Read};
use std::os::fd::BorrowedFd;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};

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

/// Bytes each guest ReadFile reads.
const CHUNK: u32 = 4 << 20;
fn guest_contents(env: &mut Env<'_>, handle: u32, then: FileOpened) -> Outcome {
    let read = match export(env, "kernel32.dll", "ReadFile") {
        Ok(read) => read,
        Err(error) => return Outcome::Fail(error),
    };
    // The chunk, then the count ReadFile writes.
    let Some(buffer) = (super::allocator().allocate)(env, CHUNK + 4) else {
        return guest_close(env, handle, move |env| then(env, Err(0x8007_000e)));
    };
    read_chunks(read, handle, buffer, Vec::new(), then)
}
fn read_chunks(
    read: u32,
    handle: u32,
    buffer: u32,
    mut bytes: Vec<u8>,
    then: FileOpened,
) -> Outcome {
    let args = [handle, buffer, CHUNK, buffer + CHUNK, 0];
    Outcome::call(read, args, move |env, result| {
        let count = env.cpu.read_u32(buffer + CHUNK).unwrap_or(0) as usize;
        let start = bytes.len();
        bytes.resize(start + count, 0);
        let copied = env.cpu.read(buffer, &mut bytes[start..]).is_ok();
        if result.value() != 0 && copied && count != 0 {
            return read_chunks(read, handle, buffer, bytes, then);
        }
        (super::allocator().free)(env, buffer);
        let contents = if result.value() == 0 || !copied {
            Err(0x8000_4005)
        } else {
            Ok((Box::new(Cursor::new(bytes)) as Contents, None))
        };
        guest_close(env, handle, move |env| then(env, contents))
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
    file(
        env,
        filename,
        Box::new(move |env, result| {
            let (file, handle) = match result {
                Ok(value) => value,
                Err(hr) => return then(env, Err(hr)),
            };
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = send.send(Decoder::open_reader(file).ok());
            });
            opened(env, handle, receive, then)
        }),
    )
}
fn opened(
    env: &mut Env<'_>,
    handle: Option<u32>,
    receive: Receiver<Option<Decoder>>,
    then: adv_dx::dshow::Opened,
) -> Outcome {
    match receive.try_recv() {
        // Undecodable contents still make a source, as the async reader does.
        Ok(decoder) => then(env, Ok((decoder, handle))),
        Err(TryRecvError::Empty) => {
            Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                opened(env, handle, receive, then)
            })
        }
        Err(TryRecvError::Disconnected) => {
            release(env, handle);
            then(env, Err(0x8000_4005))
        }
    }
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
    use std::io::{Seek, SeekFrom};
    file(
        env,
        filename,
        Box::new(move |env, result| {
            let (mut file, handle) = match result {
                Ok(value) => value,
                Err(hr) => return then(env, Err(hr)),
            };
            let sampled = ranges
                .iter()
                .map(|&(offset, length)| {
                    let mut bytes = vec![0; length];
                    file.seek(SeekFrom::Start(offset)).ok()?;
                    file.read_exact(&mut bytes).ok()?;
                    Some(bytes)
                })
                .collect();
            release(env, handle);
            then(env, Ok(sampled))
        }),
    )
}

fn read_file(env: &mut Env<'_>, filename: u32, then: adv_dx::dshow::ReadFile) -> Outcome {
    file(
        env,
        filename,
        Box::new(move |env, result| {
            let (mut file, handle) = match result {
                Ok(value) => value,
                Err(hr) => return then(env, Err(hr)),
            };
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = file
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
                    .map_err(|_| 0x8000_4005);
                let _ = send.send(result);
            });
            read_bytes(env, handle, receive, then)
        }),
    )
}
fn read_bytes(
    env: &mut Env<'_>,
    handle: Option<u32>,
    receive: Receiver<Result<Vec<u8>, u32>>,
    then: adv_dx::dshow::ReadFile,
) -> Outcome {
    match receive.try_recv() {
        Ok(Ok(bytes)) => then(env, Ok((bytes, handle))),
        Ok(Err(hr)) => {
            release(env, handle);
            then(env, Err(hr))
        }
        Err(TryRecvError::Empty) => {
            Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                read_bytes(env, handle, receive, then)
            })
        }
        Err(TryRecvError::Disconnected) => {
            release(env, handle);
            then(env, Err(0x8000_4005))
        }
    }
}
fn thread(env: &mut Env<'_>, id: u32) -> Outcome {
    let create = match export(env, "kernel32.dll", "CreateThread") {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(error),
    };
    let entry = match env
        .sys
        .thunks
        .publish(env.cpu, QUARTZ, "<DirectShowStreamThread>")
    {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    Outcome::call(create, [0, 0, entry, id, 0, 0], |env, result| {
        close(env, result.value());
        Outcome::Return(0)
    })
}
fn post(env: &mut Env<'_>, args: [u32; 4]) {
    adv_drv::user::post_message(env.ext, args);
}
