//! Native media fronts, with Wine kept under separate names for other exports
//! and classes. Files still open through Wine and retain its sharing lock.
use std::fs::File;
use std::io::Read;
use std::os::fd::BorrowedFd;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};

use adv_abi::dll::{Export, Import, ModuleImage};
use adv_abi::{Conv, Registry};
use adv_cpu::backend::Backend;
use adv_device::media::Decoder;
use adv_device::surface::SurfaceStore;
use adv_machine::{Env, Machine, Outcome, Wait};
use adv_nt::process::{module_base, module_export};
use adv_pe::{ImportModule, ImportName};

const QUARTZ: &str = "quartz.dll";
const WMVCORE: &str = "wmvcore.dll";
const QASF: &str = "qasf.dll";
const ORIGINAL_QUARTZ: &str = "winequartz.dll";
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
            std::fs::copy(directory.join(module), retained).map_err(|e| e.to_string())?;
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
        let image = ModuleImage::build(machine, module, &exports, &imports)?;
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

type FileOpened = Box<dyn FnOnce(&mut Env<'_>, Result<(File, u32), u32>) -> Outcome>;
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
                return then(env, Err(0x8004_0216));
            }
            let result = adv_nt::server::handle_fd(env, handle)
                .map_err(|_| 0x8000_4005u32)
                .and_then(|fd| {
                    // SAFETY: the server owns this fd; duplication transfers only the clone.
                    let fd = unsafe { BorrowedFd::borrow_raw(fd.fd) }
                        .try_clone_to_owned()
                        .map_err(|_| 0x8000_4005u32)?;
                    Ok((File::from(fd), handle))
                });
            if result.is_err() {
                close(env, handle);
            }
            then(env, result)
        },
    )
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
                let _ = send.send(Decoder::open_reader(file).map_err(|_| 0x8004_0265));
            });
            opened(env, handle, receive, then)
        }),
    )
}
fn opened(
    env: &mut Env<'_>,
    handle: u32,
    receive: Receiver<Result<Decoder, u32>>,
    then: adv_dx::dshow::Opened,
) -> Outcome {
    match receive.try_recv() {
        Ok(Ok(decoder)) => then(env, Ok((decoder, handle))),
        Ok(Err(hr)) => {
            close(env, handle);
            then(env, Err(hr))
        }
        Err(TryRecvError::Empty) => {
            Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                opened(env, handle, receive, then)
            })
        }
        Err(TryRecvError::Disconnected) => {
            close(env, handle);
            then(env, Err(0x8000_4005))
        }
    }
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
    handle: u32,
    receive: Receiver<Result<Vec<u8>, u32>>,
    then: adv_dx::dshow::ReadFile,
) -> Outcome {
    match receive.try_recv() {
        Ok(Ok(bytes)) => then(env, Ok((bytes, handle))),
        Ok(Err(hr)) => {
            close(env, handle);
            then(env, Err(hr))
        }
        Err(TryRecvError::Empty) => {
            Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                read_bytes(env, handle, receive, then)
            })
        }
        Err(TryRecvError::Disconnected) => {
            close(env, handle);
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
