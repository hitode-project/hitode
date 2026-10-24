//! File sources open through the ordinary guest file frontend. The decoder owns
//! its reader while the common file handle retains the sharing contract.
use adv_device::media::Decoder;
use adv_os::vfs::{FileObj, FileStream};

use super::*;
use crate::dshow::filter::{Role, Source};

const CLSID_ASYNC_READER: Iid = amovie(0xe436_ebb5);
type Opened = Box<dyn FnOnce(&mut Env<'_>, Result<(Decoder, u32), u32>) -> Outcome>;

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
            let result = Decoder::open_reader(reader).map_err(|error| {
                adv_abi::warn_unavailable(
                    env,
                    "IGraphBuilder::AddSourceFilter",
                    &error.to_string(),
                );
                VFW_E_UNSUPPORTED_STREAM
            });
            match result {
                Ok(decoder) => then(env, Ok((decoder, handle))),
                Err(error) => {
                    host::close(env, handle);
                    then(env, Err(error))
                }
            }
        },
    )
}

fn attach(env: &mut Env<'_>, filter: u32, decoder: Decoder, handle: u32, filename: Vec<u16>) {
    env.ext.get_or_default::<Playback>().sources += 1;
    let info = decoder.info().clone();
    let source = Source {
        file_handle: Some(handle),
        duration_us: info.duration_us,
        decoder: Some(decoder),
        info: Some(info),
    };
    let filter = state(env).filters.get_mut(&filter).unwrap();
    filter.source_path = filename;
    filter.source = Some(source);
    let output = filter.pins[0];
    state(env).pins.get_mut(&output).unwrap().mt = Some(MediaType {
        major: MEDIATYPE_STREAM,
        sub: GUID_NULL,
        fixed_size: false,
        temporal: true,
        sample_size: 0,
        format_type: GUID_NULL,
        format: Vec::new(),
    });
}

pub(super) fn add(env: &mut Env<'_>, graph: u32, filename: u32, name: u32, output: u32) -> Outcome {
    if output == 0 || !env.cpu.accessible(output, 4, Prot::WRITE) {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    let filename_text = match path(env, filename) {
        Ok(text) => text,
        Err(error) => return Outcome::Return(error),
    };
    let name_text = if name == 0 {
        filename_text.clone()
    } else {
        match path(env, name) {
            Ok(text) => text,
            Err(error) => return Outcome::Return(error),
        }
    };
    open(
        env,
        filename,
        Box::new(move |env, result| {
            let (decoder, handle) = match result {
                Ok(source) => source,
                Err(error) => return Outcome::Return(error),
            };
            let filter = match filter::new_filter(env, Role::FileSource, CLSID_ASYNC_READER) {
                Ok(filter) => filter,
                Err(error) => {
                    host::close(env, handle);
                    return Outcome::Return(error);
                }
            };
            attach(env, filter, decoder, handle, filename_text);
            if env.cpu.write_u32(output, filter).is_err() {
                release(env, filter);
                return Outcome::Return(E_POINTER);
            }
            // The initial reference belongs to the caller; the graph owns another.
            add_ref(env, filter);
            let source = state(env).filters.get_mut(&filter).unwrap();
            source.graph = graph;
            source.name = name_text.clone();
            g(env, graph).members.push(Member {
                filter,
                name: name_text,
                host: Some(filter),
            });
            Outcome::Return(S_OK)
        }),
    )
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
                    Ok((decoder, handle)) => {
                        attach(env, filter, decoder, handle, text);
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
pub(super) mod tests;
