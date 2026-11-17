//! File sources open through the ordinary guest file frontend. The decoder owns
//! its reader while the common file handle retains the sharing contract.
use adv_device::media::Decoder;
use adv_os::vfs::{FileObj, FileStream};

use super::*;
use crate::dshow::filter::{Role, Source};
use crate::dshow::host::{Host, MediaEntry, Opened};

mod registered;

const CLSID_ASYNC_READER: Iid = amovie(0xe436_ebb5);

fn path(env: &Env<'_>, pointer: u32) -> Result<Vec<u16>, u32> {
    if pointer == 0 {
        return Err(E_POINTER);
    }
    let mut text = Vec::new();
    for index in 0..32768u32 {
        let address = pointer.checked_add(index * 2).ok_or(E_POINTER)?;
        let unit = env.cpu.read_u16(address).map_err(|_| E_POINTER)?;
        if unit == 0 {
            return if text.is_empty() {
                Err(VFW_E_NOT_FOUND)
            } else {
                Ok(text)
            };
        }
        text.push(unit);
    }
    Err(E_INVALIDARG)
}

fn open(env: &mut Env<'_>, filename: u32, then: Opened) -> Outcome {
    if let Some(host) = env.ext.try_get::<Host>() {
        return (host.open)(env, filename, then);
    }
    if !Decoder::available() {
        adv_abi::warn_unavailable(
            env,
            "IGraphBuilder::AddSourceFilter",
            "movie decoding is disabled (build with the media feature)",
        );
        return then(env, Err(VFW_E_UNSUPPORTED_STREAM));
    }
    let target = match env
        .sys
        .thunks
        .publish(env.cpu, "kernel32.dll", "CreateFileW")
    {
        Ok(target) => target,
        Err(_) => return then(env, Err(E_NOTIMPL)),
    };
    Outcome::call(
        target,
        vec![filename, 0x8000_0000, 1, 0, 3, 0x80, 0],
        move |env, resume| {
            let handle = resume.value();
            if handle == u32::MAX {
                return then(env, Err(VFW_E_NOT_FOUND));
            }
            // Keep the ordinary FileObj for share/read/delete checks. The decoder
            // reads a clone; neither frontend ever exposes this private handle.
            let reader = env
                .sys
                .objects
                .host::<FileObj>(handle)
                .ok_or(E_UNEXPECTED)
                .and_then(|file| {
                    if let Some(bytes) = file.file.memory_bytes() {
                        Ok(FileStream::read_only(bytes.clone()))
                    } else {
                        file.file
                            .host()
                            .ok_or(E_UNEXPECTED)?
                            .try_clone()
                            .map(FileStream::from)
                            .map_err(|_| 0x8000_4005)
                    }
                });
            let reader = match reader {
                Ok(reader) => reader,
                Err(error) => {
                    host::close(env, handle);
                    return then(env, Err(error));
                }
            };
            let decoder = Decoder::open_reader(reader)
                .inspect_err(|error| {
                    adv_abi::warn_unavailable(
                        env,
                        "IGraphBuilder::AddSourceFilter",
                        &error.to_string(),
                    );
                })
                .ok();
            then(env, Ok((decoder, Some(handle))))
        },
    )
}

/// Give `filter` the opened file. Undecodable contents still make a source,
/// as the async reader they stand for does; rendering it then fails.
fn attach(
    env: &mut Env<'_>,
    filter: u32,
    decoder: Option<Decoder>,
    handle: Option<u32>,
    filename: Vec<u16>,
    entry: &MediaEntry,
) {
    env.ext.get_or_default::<Playback>().sources += 1;
    let info = decoder.as_ref().map(|d| d.info().clone());
    let source = Source {
        file_handle: handle,
        duration_us: info.as_ref().map_or(0, |i| i.duration_us),
        decoder,
        info,
    };
    let filter = state(env).filters.get_mut(&filter).unwrap();
    filter.source_path = filename;
    filter.source = Some(source);
    let output = filter.pins[0];
    state(env).pins.get_mut(&output).unwrap().mt = Some(MediaType {
        major: entry.major.unwrap_or(MEDIATYPE_STREAM),
        sub: entry.sub.unwrap_or(GUID_NULL),
        fixed_size: false,
        temporal: true,
        sample_size: 0,
        format_type: GUID_NULL,
        format: Vec::new(),
    });
}

/// IGraphBuilder::AddSourceFilter: the registry picks the source filter. The
/// async reader is the host source; any other registered source is created
/// and loaded as quartz does. Failures leave `output` untouched.
pub(super) fn add(env: &mut Env<'_>, graph: u32, filename: u32, name: u32, output: u32) -> Outcome {
    if output == 0 || !env.cpu.accessible(output, 4, Prot::WRITE) {
        return Outcome::Return(E_POINTER);
    }
    let filename_text = match path(env, filename) {
        Ok(text) => text,
        Err(error) => return Outcome::Return(error),
    };
    // Windows names a nameless source after its file.
    let name = if name == 0 { filename } else { name };
    let name_text = match path(env, name) {
        Ok(text) => text,
        Err(error) => return Outcome::Return(error),
    };
    let text = filename_text.clone();
    classify(
        env,
        filename,
        &text,
        Box::new(move |env, entry| {
            let entry = entry.unwrap_or_default();
            match entry.source.filter(|&class| class != CLSID_ASYNC_READER) {
                Some(class) => registered(env, graph, class, filename, name, output),
                None => {
                    let file = (filename_text, name_text);
                    reader(env, graph, filename, file, entry, output)
                }
            }
        }),
    )
}

type Classified = Box<dyn FnOnce(&mut Env<'_>, Option<MediaEntry>) -> Outcome>;

/// The registered entry for a file: its extension's, whose source filter
/// quartz ignores, else the first whose byte pattern matches the contents.
fn classify(env: &mut Env<'_>, filename: u32, text: &[u16], then: Classified) -> Outcome {
    let Some(host) = env.ext.try_get::<Host>().copied() else {
        return then(env, None);
    };
    let text = String::from_utf16_lossy(text);
    let extension = text.rfind('.').map(|at| &text[at..]);
    let types = (host.media_types)(env, extension);
    if let Some(entry) = types.extension {
        return then(
            env,
            Some(MediaEntry {
                source: None,
                ..entry
            }),
        );
    }
    let entries = types.entries;
    let patterns: Vec<(usize, Vec<registered::Tuple>)> = entries
        .iter()
        .enumerate()
        .flat_map(|(at, entry)| {
            entry
                .patterns
                .iter()
                .filter_map(|pattern| registered::parse(pattern))
                .map(move |tuples| (at, tuples))
        })
        .collect();
    let mut ranges: Vec<(u64, usize)> = patterns
        .iter()
        .flat_map(|(_, tuples)| tuples.iter().map(|t| (t.offset, t.mask.len())))
        .collect();
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.is_empty() {
        return then(env, None);
    }
    let requested = ranges.clone();
    (host.sample)(
        env,
        filename,
        requested,
        Box::new(move |env, sampled| {
            // An unreadable file falls to the async reader, whose Load reports it.
            let Ok(sampled) = sampled else {
                return then(env, None);
            };
            let mut read = |offset: u64, length: usize| {
                let at = ranges.binary_search(&(offset, length)).ok()?;
                sampled.get(at).cloned().flatten()
            };
            let found = patterns
                .iter()
                .find(|(_, tuples)| registered::matches(tuples, &mut read))
                .map(|&(at, _)| entries[at].clone());
            then(env, found)
        }),
    )
}

/// The host source standing for the async reader.
fn reader(
    env: &mut Env<'_>,
    graph: u32,
    filename: u32,
    (filename_text, name_text): (Vec<u16>, Vec<u16>),
    entry: MediaEntry,
    output: u32,
) -> Outcome {
    open(
        env,
        filename,
        Box::new(move |env, result| {
            let (decoder, handle) = match result {
                Ok(source) => source,
                Err(error) => return Outcome::Return(error),
            };
            let close = |env: &mut Env<'_>| {
                if let Some(handle) = handle {
                    host::close(env, handle);
                }
            };
            let Some(name_text) = g(env, graph).unique_name(Some(name_text)) else {
                close(env);
                return Outcome::Return(VFW_E_DUPLICATE_NAME);
            };
            let filter = match filter::new_filter(env, Role::FileSource, CLSID_ASYNC_READER) {
                Ok(filter) => filter,
                Err(error) => {
                    close(env);
                    return Outcome::Return(error);
                }
            };
            attach(env, filter, decoder, handle, filename_text, &entry);
            if env.cpu.write_u32(output, filter).is_err() {
                release(env, filter);
                return Outcome::Return(E_POINTER);
            }
            // The initial reference belongs to the caller; the graph owns another.
            add_ref(env, filter);
            let source = state(env).filters.get_mut(&filter).unwrap();
            source.graph = graph;
            source.name = name_text.clone();
            g(env, graph).join(Member {
                filter,
                name: name_text,
                host: Some(filter),
                seeking: 0,
            });
            Outcome::Return(S_OK)
        }),
    )
}

const CLSCTX_INPROC_SERVER: u32 = 1;

/// A registered guest source filter: CoCreateInstance, IFileSourceFilter::Load
/// with the file name, then AddFilter; the caller receives the creation reference.
fn registered(
    env: &mut Env<'_>,
    graph: u32,
    class: Iid,
    filename: u32,
    name: u32,
    output: u32,
) -> Outcome {
    let create = env
        .ext
        .try_get::<Host>()
        // ole32 forwards it to combase, which implements it.
        .and_then(|host| (host.export)(env, "combase.dll", "CoCreateInstance"));
    let Some(create) = create else {
        return Outcome::Return(REGDB_E_CLASSNOTREG);
    };
    let Some(scratch) = alloc(env, 40) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let written = env
        .cpu
        .write(scratch, &class)
        .and_then(|()| env.cpu.write(scratch + 16, &IID_IBASEFILTER));
    if written.is_err() {
        free(env, scratch);
        return Outcome::Return(E_OUTOFMEMORY);
    }
    let args = vec![scratch, 0, CLSCTX_INPROC_SERVER, scratch + 16, scratch + 32];
    Outcome::call(create, args, move |env, resume| {
        let filter = env.cpu.read_u32(scratch + 32).unwrap_or(0);
        let created = resume.value();
        if created != S_OK
            || filter == 0
            || env.cpu.write(scratch + 16, &IID_IFILESOURCEFILTER).is_err()
        {
            free(env, scratch);
            return Outcome::Return(if created == S_OK {
                E_NOINTERFACE
            } else {
                created
            });
        }
        let fail = move |env: &mut Env<'_>, hr: u32| {
            guest_call(env, filter, 2, vec![], move |_, _| Outcome::Return(hr))
        };
        guest_call(
            env,
            filter,
            0,
            vec![scratch + 16, scratch + 36],
            move |env, hr| {
                let source = env.cpu.read_u32(scratch + 36).unwrap_or(0);
                free(env, scratch);
                if hr != S_OK || source == 0 {
                    return fail(env, if hr == S_OK { E_NOINTERFACE } else { hr });
                }
                guest_call(env, source, 3, vec![filename, 0], move |env, loaded| {
                    guest_call(env, source, 2, vec![], move |env, _| {
                        if loaded & 0x8000_0000 != 0 {
                            return fail(env, loaded);
                        }
                        super::add_filter_then(env, graph, filter, name, move |env, added| {
                            if added & 0x8000_0000 != 0 {
                                return fail(env, added);
                            }
                            let _ = env.cpu.write_u32(output, filter);
                            Outcome::Return(S_OK)
                        })
                    })
                })
            },
        )
    })
}

pub(crate) fn file_method(env: &mut Env<'_>, filter: u32, method: &str) -> Outcome {
    if state(env).filters[&filter].role != Role::FileSource {
        return Outcome::Return(E_NOINTERFACE);
    }
    match method {
        "Load" => {
            if !state(env).filters[&filter].source_path.is_empty()
                || state(env).filters[&filter].state != STOPPED
            {
                return Outcome::Return(VFW_E_WRONG_STATE);
            }
            let filename = env.arg(1);
            let text = match path(env, filename) {
                Ok(text) => text,
                Err(error) => return Outcome::Return(error),
            };
            open(
                env,
                filename,
                Box::new(move |env, result| match result {
                    // The ASF reader parses the file as it loads.
                    Ok((None, handle)) => {
                        if let Some(handle) = handle {
                            host::close(env, handle);
                        }
                        Outcome::Return(VFW_E_UNSUPPORTED_STREAM)
                    }
                    Ok((decoder, handle)) => {
                        attach(env, filter, decoder, handle, text, &MediaEntry::default());
                        Outcome::Return(S_OK)
                    }
                    Err(error) => Outcome::Return(error),
                }),
            )
        }
        "GetCurFile" => {
            let (output, mt) = (env.arg(1), env.arg(2));
            if output == 0 || !env.cpu.accessible(output, 4, Prot::WRITE) {
                return Outcome::Return(E_POINTER);
            }
            let _ = env.cpu.write_u32(output, 0);
            let source = &state(env).filters[&filter];
            let mut text = source.source_path.clone();
            let pin = source.pins[0];
            let media_type = state(env).pins[&pin].mt.clone();
            if mt != 0 && !env.cpu.accessible(mt, MEDIA_TYPE_SIZE, Prot::WRITE) {
                return Outcome::Return(E_POINTER);
            }
            text.push(0);
            let bytes: Vec<u8> = text.iter().flat_map(|unit| unit.to_le_bytes()).collect();
            let pointer = adv_os::com::task_alloc(env, bytes.len() as u32);
            if pointer == 0 {
                return Outcome::Return(E_OUTOFMEMORY);
            }
            let _ = env.cpu.write(pointer, &bytes);
            if mt != 0 {
                let result = match media_type {
                    Some(mt_value) => mt_value.write(env, mt),
                    None => env
                        .cpu
                        .write(mt, &[0; MEDIA_TYPE_SIZE as usize])
                        .map_err(|_| E_POINTER),
                };
                if let Err(error) = result {
                    adv_os::com::task_free(env, pointer);
                    return Outcome::Return(error);
                }
            }
            let _ = env.cpu.write_u32(output, pointer);
            Outcome::Return(S_OK)
        }
        _ => Outcome::Return(E_NOTIMPL),
    }
}

#[cfg(test)]
pub(crate) mod tests;
