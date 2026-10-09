//! Whole-module vorbisfile provider. Datasources stay opaque guest values:
//! every read, seek, tell and close goes through the supplied guest callbacks.
//! The Ogg parser accepts complete pages, avoiding synchronous CPU re-entry
//! from a host Read implementation. PCM decoding is deferred until ov_read.
use std::collections::BTreeSet;

use adv_abi::Registry;
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::vorbis::{ByteOrder, DecodeError, Next, Packet, PcmFormat, Stream as CodecStream};
use adv_machine::{Conv, Env, Identity, Machine, Outcome};
use adv_os::FileSource;

mod store;
use store::{Provider, StreamSlot};

const READ_SIZE: u32 = 65_536;
const ARENA_SIZE: u32 = 131_072;
const VF_PREFIX: usize = 0x64;
const OV_EREAD: i32 = -128;
const OV_EFAULT: i32 = -129;
const OV_EINVAL: i32 = -131;
const OV_ENOTVORBIS: i32 = -132;
const OV_EBADHEADER: i32 = -133;
const OV_EBADLINK: i32 = -137;
const OV_ENOSEEK: i32 = -138;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    #[default]
    Guest,
    Native,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub policy: Policy,
    pub reason: Option<String>,
}
impl Selection {
    fn guest(reason: impl Into<String>) -> Self {
        Self {
            policy: Policy::Guest,
            reason: Some(reason.into()),
        }
    }
}

// Export ordinals follow the standard older vorbisfile DLL ABI. Unsupported
// functions are deliberately absent: preflight selects the complete guest DLL.
const EXPORTS: &[(&str, u8, u16)] = &[
    ("ov_clear", 1, 3),
    ("ov_comment", 2, 4),
    ("ov_info", 2, 5),
    ("ov_open_callbacks", 8, 7),
    ("ov_pcm_seek", 3, 8),
    ("ov_pcm_tell", 1, 10),
    ("ov_pcm_total", 2, 11),
    ("ov_raw_tell", 1, 13),
    ("ov_raw_total", 2, 14),
    ("ov_read", 7, 15),
    ("ov_seekable", 1, 16),
    ("ov_serialnumber", 2, 17),
    ("ov_streams", 1, 18),
    ("ov_test_callbacks", 8, 20),
    ("ov_test_open", 1, 21),
    ("ov_time_seek", 3, 22),
    ("ov_time_tell", 1, 24),
    ("ov_time_total", 2, 25),
];

/// Select one provider before creating any decoder. Direct libvorbis/libogg
/// imports or an unsupported vorbisfile export keep the entire guest codec.
pub fn select_policy(
    files: &dyn FileSource,
    executable: &str,
    requested: Policy,
) -> Result<Policy, String> {
    Ok(select_policy_with_reason(files, executable, requested)?.policy)
}

pub fn select_policy_with_reason(
    files: &dyn FileSource,
    executable: &str,
    requested: Policy,
) -> Result<Selection, String> {
    if requested == Policy::Guest {
        return Ok(Selection::guest("explicit Guest policy"));
    }
    let lower_symbols = lower_symbols(files);
    let mut verified_ordinal_layout = false;
    if let Some((path, bytes)) = adv_os::loader::module_file(files, "vorbisfile.dll", None) {
        if let Some(reason) = ordinal_layout_error(&bytes) {
            return Ok(Selection::guest(format!("{path}: {reason}")));
        }
        verified_ordinal_layout = true;
    }
    let mut pending = vec![executable.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.to_ascii_lowercase()) {
            continue;
        }
        let Some(bytes) = files.read(&path) else {
            continue;
        };
        // GetProcAddress names are often not in the PE import table. A
        // conservative literal-name check prevents unsupported dynamic calls
        // from meeting host-created vf state later in the same process.
        if let Some(symbol) = unsupported_dynamic_symbol(&bytes, &lower_symbols) {
            return Ok(Selection::guest(format!(
                "{path}: unsupported dynamic codec symbol literal {symbol}"
            )));
        }
        // This is a capability proof for replacing the complete codec, not the
        // process loader. Packed or otherwise uninspectable dependencies cannot
        // prove the Native ABI safe. Leave them and all codec state to Guest;
        // the loader still reports errors in required image/import data itself.
        let pe = match adv_pe::Pe::parse(&bytes) {
            Ok(pe) => pe,
            Err(e) => {
                return Ok(Selection::guest(format!(
                    "codec preflight {path}: PE headers/image unavailable: {e}"
                )));
            }
        };
        let normal = match pe.imports() {
            Ok(imports) => imports,
            Err(e) => {
                return Ok(Selection::guest(format!(
                    "codec preflight {path}: imports unavailable: {e}"
                )));
            }
        };
        let delay = match pe.delay_imports() {
            Ok(imports) => imports,
            Err(e) => {
                return Ok(Selection::guest(format!(
                    "codec preflight {path}: delay imports unavailable: {e}"
                )));
            }
        };
        let mut imports: Vec<_> = normal.into_iter().map(|m| (m.dll, m.imports)).collect();
        imports.extend(delay.into_iter().map(|m| (m.dll, m.imports)));
        for (dll, symbols) in imports {
            let key = dll
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&dll)
                .to_ascii_lowercase();
            if matches!(key.as_str(), "vorbis.dll" | "ogg.dll") {
                return Ok(Selection::guest(format!(
                    "{path}: direct lower codec import {dll}"
                )));
            }
            if key == "vorbisfile.dll" {
                if !verified_ordinal_layout {
                    let dir = path.rfind(['/', '\\']).map(|i| &path[..i]);
                    if let Some((codec_path, bytes)) = adv_os::loader::module_file(files, &dll, dir)
                    {
                        if let Some(reason) = ordinal_layout_error(&bytes) {
                            return Ok(Selection::guest(format!("{codec_path}: {reason}")));
                        }
                        verified_ordinal_layout = true;
                    }
                }
                for import in symbols {
                    let description = import.name.to_string();
                    let supported = match import.name {
                        adv_pe::ImportName::Name { name, .. } => {
                            EXPORTS.iter().any(|&(n, _, _)| n == name)
                        }
                        adv_pe::ImportName::Ordinal(ordinal) => {
                            verified_ordinal_layout && EXPORTS.iter().any(|&(_, _, o)| o == ordinal)
                        }
                    };
                    if !supported {
                        return Ok(Selection::guest(format!(
                            "{path}: unsupported or unverified import {dll}!{description}"
                        )));
                    }
                }
            } else {
                let dir = path.rfind(['/', '\\']).map(|i| &path[..i]);
                if let Some((dependency_path, _)) = adv_os::loader::module_file(files, &dll, dir) {
                    pending.push(dependency_path);
                }
            }
        }
    }
    Ok(Selection {
        policy: Policy::Native,
        reason: None,
    })
}

fn ordinal_layout_error(bytes: &[u8]) -> Option<String> {
    let pe = match adv_pe::Pe::parse(bytes) {
        Ok(pe) => pe,
        Err(e) => return Some(format!("invalid codec PE: {e}")),
    };
    let exports = match pe.exports() {
        Ok(exports) => exports,
        Err(e) => return Some(format!("invalid codec exports: {e}")),
    };
    for &(name, _, ordinal) in EXPORTS {
        let actual = exports.iter().find(|export| export.ordinal == ordinal);
        if actual.is_none_or(|export| {
            export.name.as_deref() != Some(name)
                || !matches!(export.target, adv_pe::ExportTarget::Rva(_))
        }) {
            return Some(format!(
                "ordinal #{ordinal} expected {name}, actual {}",
                actual
                    .and_then(|e| e.name.as_deref())
                    .unwrap_or("<missing/unnamed>")
            ));
        }
    }
    None
}

// Public function names from Xiph's ogg.h and codec.h. A resource key or
// configuration field beginning with ogg_/vorbis_ is not evidence of a call.
const LOWER_APIS:&str="ogg_stream_packetin ogg_stream_iovecin ogg_stream_pageout ogg_stream_pageout_fill
ogg_stream_flush ogg_stream_flush_fill ogg_sync_init ogg_sync_clear ogg_sync_reset ogg_sync_destroy
ogg_sync_check ogg_sync_buffer ogg_sync_wrote ogg_sync_pageseek ogg_sync_pageout ogg_stream_pagein
ogg_stream_packetout ogg_stream_packetpeek ogg_stream_init ogg_stream_clear ogg_stream_reset
ogg_stream_reset_serialno ogg_stream_destroy ogg_stream_check ogg_stream_eos ogg_page_checksum_set
ogg_page_version ogg_page_continued ogg_page_bos ogg_page_eos ogg_page_granulepos ogg_page_serialno
ogg_page_pageno ogg_page_packets ogg_packet_clear vorbis_info_init vorbis_info_clear
vorbis_comment_init vorbis_comment_add vorbis_comment_add_tag vorbis_comment_query
vorbis_comment_query_count vorbis_comment_clear vorbis_block_init vorbis_block_clear vorbis_dsp_clear
vorbis_granule_time vorbis_version_string vorbis_analysis_init vorbis_commentheader_out
vorbis_analysis_headerout vorbis_analysis_buffer vorbis_analysis_wrote vorbis_analysis_blockout
vorbis_analysis vorbis_bitrate_addblock vorbis_bitrate_flushpacket vorbis_synthesis_idheader
vorbis_synthesis_headerin vorbis_synthesis_init vorbis_synthesis_restart vorbis_synthesis
vorbis_synthesis_trackonly vorbis_synthesis_blockin vorbis_synthesis_pcmout vorbis_synthesis_lapout
vorbis_synthesis_read vorbis_synthesis_halfrate vorbis_synthesis_halfrate_p vorbis_packet_blocksize
vorbis_info_blocksize";
fn lower_symbols(files: &dyn FileSource) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = LOWER_APIS
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect();
    for prefix in ["oggpack_", "oggpackB_"] {
        for suffix in [
            "writeinit",
            "writecheck",
            "writetrunc",
            "writealign",
            "writecopy",
            "reset",
            "writeclear",
            "readinit",
            "write",
            "look",
            "look1",
            "adv",
            "adv1",
            "read",
            "read1",
            "bytes",
            "bits",
            "get_buffer",
        ] {
            names.insert(format!("{prefix}{suffix}"));
        }
    }
    for dll in ["vorbis.dll", "ogg.dll"] {
        if let Some((_, bytes)) = adv_os::loader::module_file(files, dll, None)
            && let Ok(pe) = adv_pe::Pe::parse(&bytes)
            && let Ok(exports) = pe.exports()
        {
            names.extend(exports.into_iter().filter_map(|e| e.name));
        }
    }
    names
}
fn unsupported_dynamic_symbol<'a>(bytes: &'a [u8], lower: &BTreeSet<String>) -> Option<&'a str> {
    bytes
        .split(|&byte| byte == 0)
        .filter_map(|literal| std::str::from_utf8(literal).ok())
        .find(|&name| {
            matches!(name, "vorbis.dll" | "ogg.dll")
                || lower.contains(name)
                || name.starts_with("ov_")
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && !EXPORTS.iter().any(|&(export, _, _)| export == name)
        })
}

pub fn register(registry: &mut Registry, policy: Policy) {
    if policy == Policy::Guest {
        return;
    }
    registry.replace_module("vorbisfile.dll");
    for (index, &(name, argc, ordinal)) in EXPORTS.iter().enumerate() {
        registry.add_with_data(
            "vorbisfile.dll",
            name,
            Conv::Cdecl { argc },
            call,
            index as u64,
        );
        registry.add_with_data(
            "vorbisfile.dll",
            &format!("#{ordinal}"),
            Conv::Cdecl { argc },
            call,
            index as u64,
        );
    }
}

pub fn install<B: Backend>(machine: &mut Machine<B>, policy: Policy) -> Result<(), String> {
    if policy == Policy::Native {
        machine.ext.insert(Provider::default());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub opens: u64,
    pub failed_opens: u64,
    pub clears: u64,
    pub active_streams: u64,
    pub decoded_packets: u64,
    pub decoded_frames: u64,
    pub guest_callbacks: u64,
    pub read_callbacks: u64,
    pub seek_callbacks: u64,
    pub tell_callbacks: u64,
    pub close_callbacks: u64,
    pub callback_bytes: u64,
    pub metadata_scans: u64,
}

pub fn stats<B: Backend>(machine: &Machine<B>) -> Option<Stats> {
    machine.ext.try_get::<Provider>().map(|p| Stats {
        active_streams: p.count(&machine.sys.objects) as u64,
        ..p.stats
    })
}

#[derive(Clone, Copy)]
enum Callback {
    Read,
    Seek,
    Tell,
    Close,
}
fn callback(
    e: &mut Env<'_>,
    kind: Callback,
    target: u32,
    args: impl Into<Vec<u32>>,
    then: impl FnOnce(&mut Env<'_>, adv_machine::Resume) -> Outcome + 'static,
) -> Outcome {
    let stats = &mut e.ext.get_mut::<Provider>().stats;
    stats.guest_callbacks += 1;
    match kind {
        Callback::Read => stats.read_callbacks += 1,
        Callback::Seek => stats.seek_callbacks += 1,
        Callback::Tell => stats.tell_callbacks += 1,
        Callback::Close => stats.close_callbacks += 1,
    }
    Outcome::call(target, args, then)
}

struct Stream {
    datasource: u32,
    callbacks: [u32; 4],
    arena: u32,
    seekable: bool,
    partial: bool,
    busy: bool,
    codec: CodecStream,
    comment_memory: Vec<u8>,
}

#[derive(Clone)]
enum Operation {
    Open,
    Read {
        output: u32,
        length: u32,
        endian: bool,
        word: u32,
        signed: bool,
        section: u32,
    },
    Seek {
        target: u64,
    },
}

fn error(code: i32) -> Outcome {
    Outcome::Return(code as u32)
}
fn total_error(name: &str, code: i32) -> Outcome {
    if matches!(name, "ov_time_total" | "ov_time_tell") {
        Outcome::ReturnF64(f64::from(code))
    } else {
        Outcome::Return64(code as i64 as u64)
    }
}

fn call(e: &mut Env<'_>) -> Outcome {
    let name = EXPORTS[e.sys.thunks.get(e.thunk).f.data as usize].0;
    let vf = e.arg(0);
    if matches!(
        name,
        "ov_read" | "ov_clear" | "ov_pcm_seek" | "ov_time_seek" | "ov_test_open"
    ) && e
        .ext
        .get::<Provider>()
        .get(&e.sys.objects, vf)
        .is_some_and(|s| s.busy)
    {
        return error(OV_EINVAL);
    }
    match name {
        "ov_open_callbacks" | "ov_test_callbacks" => open(e, name == "ov_test_callbacks"),
        "ov_clear" => clear(e, vf),
        "ov_read" => {
            let word = e.arg(4);
            let length = e.arg(2);
            if !matches!(word, 1 | 2) || length > i32::MAX as u32 {
                return error(OV_EINVAL);
            }
            advance(
                e,
                vf,
                Operation::Read {
                    output: e.arg(1),
                    length,
                    endian: e.arg(3) != 0,
                    word,
                    signed: e.arg(5) != 0,
                    section: e.arg(6),
                },
            )
        }
        "ov_pcm_seek" | "ov_time_seek" => {
            let bits = u64::from(e.arg(1)) | (u64::from(e.arg(2)) << 32);
            let Some(s) = e.ext.get::<Provider>().get(&e.sys.objects, vf) else {
                return error(OV_EINVAL);
            };
            if s.partial {
                return error(OV_EINVAL);
            }
            let target = if name == "ov_time_seek" {
                let seconds = f64::from_bits(bits);
                if !seconds.is_finite() || seconds < 0.0 {
                    return error(OV_EINVAL);
                }
                (seconds * f64::from(s.codec.format().sample_rate)).floor() as u64
            } else {
                bits
            };
            seek(e, vf, target)
        }
        "ov_time_total" | "ov_pcm_total" | "ov_raw_total" => {
            totals(e, vf, e.arg(1) as i32, name.to_owned())
        }
        _ => {
            let link = e.arg(1) as i32;
            let Some(s) = e.ext.get::<Provider>().get_mut(&mut e.sys.objects, vf) else {
                return if matches!(name, "ov_time_tell" | "ov_pcm_tell" | "ov_raw_tell") {
                    total_error(&name, OV_EINVAL)
                } else {
                    if matches!(
                        name,
                        "ov_info" | "ov_comment" | "ov_streams" | "ov_seekable"
                    ) {
                        Outcome::Return(0)
                    } else {
                        error(OV_EINVAL)
                    }
                };
            };
            if s.partial && matches!(name, "ov_pcm_tell" | "ov_raw_tell" | "ov_time_tell") {
                return total_error(&name, OV_EINVAL);
            }
            match name {
                "ov_test_open" => {
                    if !s.partial {
                        return error(OV_EINVAL);
                    }
                    s.partial = false;
                    publish(e, vf);
                    Outcome::Return(0)
                }
                "ov_info" => Outcome::Return(if s.seekable && link > 0 {
                    0
                } else {
                    s.arena + READ_SIZE
                }),
                "ov_comment" => Outcome::Return(if s.seekable && link > 0 {
                    0
                } else {
                    s.arena + READ_SIZE + 32
                }),
                "ov_pcm_tell" => Outcome::Return64(s.codec.pcm_position()),
                "ov_raw_tell" => Outcome::Return64(s.codec.raw_position()),
                "ov_time_tell" => Outcome::ReturnF64(
                    s.codec.pcm_position() as f64 / f64::from(s.codec.format().sample_rate),
                ),
                "ov_seekable" => Outcome::Return(u32::from(s.seekable)),
                "ov_streams" => Outcome::Return(1),
                "ov_serialnumber" => Outcome::Return(s.codec.format().serial),
                _ => error(OV_EINVAL),
            }
        }
    }
}

fn open(e: &mut Env<'_>, partial: bool) -> Outcome {
    let vf = e.arg(1);
    let initial = e.arg(2);
    let initial_len = e.arg(3);
    let callbacks = [e.arg(4), e.arg(5), e.arg(6), e.arg(7)];
    let datasource = e.arg(0);
    if vf == 0
        || callbacks[0] == 0
        || initial_len > i32::MAX as u32
        || e.ext
            .get::<Provider>()
            .identity(&e.sys.objects, vf)
            .is_some()
    {
        return error(OV_EINVAL);
    }
    let mut raw = vec![0; initial_len as usize];
    if initial_len != 0 && e.cpu.read(initial, &mut raw).is_err() {
        return error(OV_EINVAL);
    }
    if e.cpu.write(vf, &[0; VF_PREFIX]).is_err() {
        return error(OV_EFAULT);
    }
    let Ok(arena) = e.sys.vm.alloc(e.cpu, ARENA_SIZE, Prot::RW) else {
        return error(OV_EFAULT);
    };
    let inserted = e.ext.get_mut::<Provider>().insert(
        &mut e.sys.objects,
        vf,
        Stream {
            datasource,
            callbacks,
            arena,
            seekable: false,
            partial,
            busy: true,
            codec: CodecStream::new(raw),
            comment_memory: Vec::new(),
        },
    );
    if !inserted {
        let _ = e.sys.vm.free(e.cpu, arena);
        return error(OV_EFAULT);
    }
    e.ext.get_mut::<Provider>().stats.opens += 1;
    let identity = e
        .ext
        .get::<Provider>()
        .identity(&e.sys.objects, vf)
        .unwrap();
    if callbacks[1] == 0 || callbacks[3] == 0 {
        return advance(e, vf, Operation::Open);
    }
    let datasource = e.arg(0);
    callback(
        e,
        Callback::Seek,
        callbacks[1],
        [datasource, 0, 0, 1],
        move |e, r| {
            if Provider::at(&e.sys.objects, identity).is_none() {
                return error(OV_EINVAL);
            }
            if r.value() as i32 != 0 {
                return advance(e, vf, Operation::Open);
            }
            callback(
                e,
                Callback::Tell,
                callbacks[3],
                [datasource],
                move |e, r| {
                    let Some(s) = Provider::at_mut(&mut e.sys.objects, identity) else {
                        return error(OV_EINVAL);
                    };
                    if r.value() as i32 >= 0 {
                        s.seekable = true;
                        s.codec.set_source_position(u64::from(r.value()));
                    }
                    advance(e, vf, Operation::Open)
                },
            )
        },
    )
}

fn clear(e: &mut Env<'_>, vf: u32) -> Outcome {
    let Some(s) = e.ext.get::<Provider>().remove(&mut e.sys.objects, vf) else {
        return Outcome::Return(0);
    };
    e.ext.get_mut::<Provider>().stats.clears += 1;
    let _ = e.cpu.write(vf, &[0; VF_PREFIX]);
    let _ = e.sys.vm.free(e.cpu, s.arena);
    if s.callbacks[2] == 0 {
        Outcome::Return(0)
    } else {
        callback(
            e,
            Callback::Close,
            s.callbacks[2],
            [s.datasource],
            |_, _| Outcome::Return(0),
        )
    }
}

fn abandon(e: &mut Env<'_>, vf: u32, code: i32, opening: bool) -> Outcome {
    if opening && let Some(s) = e.ext.get::<Provider>().remove(&mut e.sys.objects, vf) {
        e.ext.get_mut::<Provider>().stats.failed_opens += 1;
        let _ = e.sys.vm.free(e.cpu, s.arena);
        let _ = e.cpu.write(vf, &[0; VF_PREFIX]);
        // Failed open never takes ownership and must not invoke close_func.
    }
    if !opening && let Some(s) = e.ext.get::<Provider>().get_mut(&mut e.sys.objects, vf) {
        s.busy = false;
    }
    error(code)
}

fn codec_error(error: DecodeError) -> i32 {
    match error {
        DecodeError::NotVorbis => OV_ENOTVORBIS,
        DecodeError::BadHeader => OV_EBADHEADER,
        DecodeError::BadLink => OV_EBADLINK,
        DecodeError::UnexpectedSampleFormat => OV_EFAULT,
    }
}
fn header(s: &mut Stream, packet: Packet) -> Result<(), i32> {
    // Keep guest layout bounds validation before committing the comment header.
    if s.codec.header_count() == 1 {
        s.codec.validate_header(&packet).map_err(codec_error)?;
        s.comment_memory = parse_comments(&packet.data, s.arena + READ_SIZE + 32)?;
    }
    s.codec.accept_header(packet).map_err(codec_error)
}

fn parse_comments(data: &[u8], base: u32) -> Result<Vec<u8>, i32> {
    let parsed = adv_device::vorbis::parse_comments(data).map_err(codec_error)?;
    let vendor = parsed.vendor;
    let comments = parsed.values;
    let count = comments.len();
    let pointer_offset = 112usize; // Reserve fixed vi/offset/length public arrays.
    let length_offset = pointer_offset + (count + 1) * 4;
    let strings_offset = length_offset + count * 4;
    let required =
        strings_offset + vendor.len() + 1 + comments.iter().map(|c| c.len() + 1).sum::<usize>();
    if required > READ_SIZE as usize - 32 {
        return Err(OV_EFAULT);
    }
    let mut memory = vec![0u8; required];
    memory[0..4].copy_from_slice(&(base + pointer_offset as u32).to_le_bytes());
    memory[4..8].copy_from_slice(&(base + length_offset as u32).to_le_bytes());
    memory[8..12].copy_from_slice(&(count as u32).to_le_bytes());
    memory[12..16].copy_from_slice(&(base + strings_offset as u32).to_le_bytes());
    memory[strings_offset..strings_offset + vendor.len()].copy_from_slice(&vendor);
    let mut cursor = strings_offset + vendor.len() + 1;
    for (i, comment) in comments.iter().enumerate() {
        memory[pointer_offset + i * 4..pointer_offset + (i + 1) * 4]
            .copy_from_slice(&(base + cursor as u32).to_le_bytes());
        memory[length_offset + i * 4..length_offset + (i + 1) * 4]
            .copy_from_slice(&(comment.len() as u32).to_le_bytes());
        memory[cursor..cursor + comment.len()].copy_from_slice(comment);
        cursor += comment.len() + 1;
    }
    Ok(memory)
}

fn advance(e: &mut Env<'_>, vf: u32, op: Operation) -> Outcome {
    let Some((identity, mut s)) = e.ext.get::<Provider>().take(&mut e.sys.objects, vf) else {
        return error(OV_EINVAL);
    };
    let opening = matches!(op, Operation::Open);
    s.busy = true;
    if !opening
        && (s.partial || (s.codec.header_count() != 3 && !matches!(op, Operation::Seek { .. })))
    {
        s.busy = false;
        Provider::put(&mut e.sys.objects, identity, s);
        return error(OV_EINVAL);
    }
    if let Operation::Read { length, word, .. } = &op
        && (*length as usize) < s.codec.format().channels * (*word as usize)
    {
        s.busy = false;
        Provider::put(&mut e.sys.objects, identity, s);
        return error(OV_EINVAL);
    }
    loop {
        if opening && s.codec.header_count() == 3 {
            s.busy = false;
            Provider::put(&mut e.sys.objects, identity, s);
            publish(e, vf);
            return Outcome::Return(0);
        }
        match &op {
            Operation::Read {
                output,
                length,
                endian,
                word,
                signed,
                section,
            } if s.codec.has_pcm() || *length == 0 => {
                let bytes = s.codec.prepare_pcm(
                    *length as usize,
                    PcmFormat {
                        bytes_per_sample: *word as u8,
                        byte_order: if *endian {
                            ByteOrder::Big
                        } else {
                            ByteOrder::Little
                        },
                        signed: *signed,
                    },
                );
                let count = bytes.len();
                let result = if e.cpu.write(*output, bytes).is_err()
                    || (*section != 0 && e.cpu.write_u32(*section, 0).is_err())
                {
                    error(OV_EFAULT)
                } else {
                    s.codec.consume_samples(count / *word as usize);
                    Outcome::Return(count as u32)
                };
                s.busy = false;
                Provider::put(&mut e.sys.objects, identity, s);
                publish(e, vf);
                return result;
            }
            Operation::Seek { target } if s.codec.has_pcm() => {
                if s.codec.skip_to(*target) {
                    s.busy = false;
                    Provider::put(&mut e.sys.objects, identity, s);
                    publish(e, vf);
                    return Outcome::Return(0);
                }
            }
            Operation::Seek { target: 0 } if s.codec.header_count() == 3 => {
                s.busy = false;
                Provider::put(&mut e.sys.objects, identity, s);
                publish(e, vf);
                return Outcome::Return(0);
            }
            _ => {}
        }
        match s.codec.next_packet() {
            Next::Packet(p) => {
                let result = if s.codec.header_count() < 3 {
                    header(&mut s, p)
                } else {
                    let before = s.codec.decoded_frames();
                    let result = s.codec.decode_packet(p).map_err(codec_error);
                    let stats = &mut e.ext.get_mut::<Provider>().stats;
                    stats.decoded_packets += 1;
                    stats.decoded_frames += s.codec.decoded_frames() - before;
                    result
                };
                if let Err(code) = result {
                    Provider::put(&mut e.sys.objects, identity, s);
                    return abandon(e, vf, code, opening);
                }
            }
            Next::More => {
                s.codec.compact_input();
                let (read, datasource, scratch) = (s.callbacks[0], s.datasource, s.arena);
                Provider::put(&mut e.sys.objects, identity, s);
                return callback(
                    e,
                    Callback::Read,
                    read,
                    [scratch, 1, READ_SIZE, datasource],
                    move |e, r| {
                        if Provider::at(&e.sys.objects, identity).is_none() {
                            return error(OV_EINVAL);
                        }
                        let length = r.value();
                        if length > READ_SIZE {
                            return abandon(e, vf, OV_EREAD, opening);
                        }
                        e.ext.get_mut::<Provider>().stats.callback_bytes += u64::from(length);
                        let mut bytes = vec![0; length as usize];
                        if e.cpu.read(scratch, &mut bytes).is_err() {
                            return abandon(e, vf, OV_EREAD, opening);
                        }
                        let Some(s) = Provider::at_mut(&mut e.sys.objects, identity) else {
                            return error(OV_EINVAL);
                        };
                        s.codec.feed(bytes);
                        advance(e, vf, op)
                    },
                );
            }
            Next::Eof => {
                Provider::put(&mut e.sys.objects, identity, s);
                return abandon(
                    e,
                    vf,
                    if opening {
                        OV_EBADHEADER
                    } else if matches!(op, Operation::Seek { .. }) {
                        OV_EINVAL
                    } else {
                        0
                    },
                    opening,
                );
            }
            Next::Error(code) => {
                Provider::put(&mut e.sys.objects, identity, s);
                return abandon(e, vf, codec_error(code), opening);
            }
        }
    }
}

fn publish(e: &mut Env<'_>, vf: u32) {
    let s = e.ext.get::<Provider>().get(&e.sys.objects, vf).unwrap();
    let info = s.arena + READ_SIZE;
    let mut prefix = [0u8; VF_PREFIX];
    let mut word = |offset: usize, value: u32| {
        prefix[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
    };
    word(0, s.datasource);
    word(4, u32::from(s.seekable));
    word(0x34, 1);
    word(0x38, info + 64);
    word(0x3c, info + 80);
    word(0x40, info + 88);
    word(0x44, info + 96);
    word(0x48, info);
    word(0x4c, info + 32);
    word(0x58, if s.partial { 1 } else { 2 });
    word(0x5c, s.codec.format().serial);
    let raw_offset = s.codec.raw_position();
    prefix[8..16].copy_from_slice(&raw_offset.to_le_bytes());
    prefix[16..24].copy_from_slice(&s.codec.source_length().unwrap_or(0).to_le_bytes());
    prefix[0x50..0x58].copy_from_slice(&s.codec.pcm_position().to_le_bytes());
    let mut vi = [0u8; 32];
    if let Some(header) = s.codec.identification_header() {
        vi[4..8].copy_from_slice(&(s.codec.format().channels as u32).to_le_bytes());
        vi[8..12].copy_from_slice(&s.codec.format().sample_rate.to_le_bytes());
        vi[12..24].copy_from_slice(&header[16..28]);
    }
    let _ = e.cpu.write(vf, &prefix);
    let _ = e.cpu.write(info, &vi);
    let _ = e.cpu.write(info + 32, &s.comment_memory);
    let _ = e.cpu.write(info + 64, &0u64.to_le_bytes());
    let _ = e.cpu.write(
        info + 72,
        &s.codec.source_length().unwrap_or(0).to_le_bytes(),
    );
    let _ = e
        .cpu
        .write(info + 80, &s.codec.data_position().to_le_bytes());
    let _ = e.cpu.write_u32(info + 88, s.codec.format().serial);
    let _ = e.cpu.write(info + 96, &0u64.to_le_bytes());
    let _ = e.cpu.write(
        info + 104,
        &s.codec.total_frames().unwrap_or(0).to_le_bytes(),
    );
}

fn seek(e: &mut Env<'_>, vf: u32, target: u64) -> Outcome {
    let Some(s) = e.ext.get::<Provider>().get(&e.sys.objects, vf) else {
        return error(OV_EINVAL);
    };
    if !s.seekable {
        return error(OV_ENOSEEK);
    }
    if target > i64::MAX as u64 || s.codec.total_frames().is_some_and(|total| target > total) {
        return error(OV_EINVAL);
    }
    let (seek_callback, datasource) = (s.callbacks[1], s.datasource);
    let identity = e
        .ext
        .get::<Provider>()
        .identity(&e.sys.objects, vf)
        .unwrap();
    e.ext
        .get::<Provider>()
        .get_mut(&mut e.sys.objects, vf)
        .unwrap()
        .busy = true;
    callback(
        e,
        Callback::Seek,
        seek_callback,
        [datasource, 0, 0, 0],
        move |e, r| {
            let Some(s) = Provider::at_mut(&mut e.sys.objects, identity) else {
                return error(OV_EINVAL);
            };
            if r.value() as i32 != 0 {
                s.busy = false;
                return error(OV_EREAD);
            }
            // Re-read the three headers from the datasource; no eager PCM decode.
            s.codec.reset();
            advance(e, vf, Operation::Seek { target })
        },
    )
}

fn totals(e: &mut Env<'_>, vf: u32, link: i32, name: String) -> Outcome {
    let Some(s) = e.ext.get::<Provider>().get(&e.sys.objects, vf) else {
        return total_error(&name, OV_EINVAL);
    };
    if link > 0 || !s.seekable || s.partial {
        return if name == "ov_time_total" {
            Outcome::ReturnF64(f64::from(OV_EINVAL))
        } else {
            Outcome::Return64(OV_EINVAL as i64 as u64)
        };
    }
    if let Some(total) = s
        .codec
        .total_frames()
        .filter(|_| name != "ov_raw_total" || s.codec.source_length().is_some())
    {
        return if name == "ov_time_total" {
            Outcome::ReturnF64(total as f64 / f64::from(s.codec.format().sample_rate))
        } else if name == "ov_pcm_total" {
            Outcome::Return64(total)
        } else if let Some(end) = s.codec.source_length() {
            Outcome::Return64(end)
        } else {
            error(OV_EINVAL)
        };
    }
    // Metadata scanning is implemented as callback continuations, preserving
    // the active packet/decoder state; it never synthesizes audio.
    scan_total(e, vf, name)
}

struct Scan {
    vf: u32,
    identity: Identity<StreamSlot>,
    name: String,
    datasource: u32,
    callbacks: [u32; 4],
    scratch: u32,
    saved: u64,
    end: u64,
    start: u64,
    bytes: Vec<u8>,
}

fn scan_total(e: &mut Env<'_>, vf: u32, name: String) -> Outcome {
    let s = e.ext.get::<Provider>().get(&e.sys.objects, vf).unwrap();
    if s.busy {
        return total_error(&name, OV_EINVAL);
    }
    let mut scan = Scan {
        vf,
        identity: e
            .ext
            .get::<Provider>()
            .identity(&e.sys.objects, vf)
            .unwrap(),
        name,
        datasource: s.datasource,
        callbacks: s.callbacks,
        scratch: s.arena,
        saved: 0,
        end: 0,
        start: 0,
        bytes: Vec::new(),
    };
    e.ext
        .get::<Provider>()
        .get_mut(&mut e.sys.objects, vf)
        .unwrap()
        .busy = true;
    e.ext.get_mut::<Provider>().stats.metadata_scans += 1;
    callback(
        e,
        Callback::Tell,
        scan.callbacks[3],
        [scan.datasource],
        move |e, r| {
            let Some(s) = Provider::at_mut(&mut e.sys.objects, scan.identity) else {
                return total_error(&scan.name, OV_EINVAL);
            };
            if (r.value() as i32) < 0 {
                s.busy = false;
                return total_error(&scan.name, OV_EREAD);
            }
            scan.saved = u64::from(r.value());
            callback(
                e,
                Callback::Seek,
                scan.callbacks[1],
                [scan.datasource, 0, 0, 2],
                move |e, r| {
                    if Provider::at(&e.sys.objects, scan.identity).is_none() {
                        return total_error(&scan.name, OV_EINVAL);
                    }
                    if r.value() as i32 != 0 {
                        return restore_scan(e, scan, Err(OV_EREAD));
                    }
                    callback(
                        e,
                        Callback::Tell,
                        scan.callbacks[3],
                        [scan.datasource],
                        move |e, r| {
                            if Provider::at(&e.sys.objects, scan.identity).is_none() {
                                return total_error(&scan.name, OV_EINVAL);
                            }
                            if (r.value() as i32) < 0 {
                                return restore_scan(e, scan, Err(OV_EREAD));
                            }
                            scan.end = u64::from(r.value());
                            // Two maximum Ogg pages cover an EOS page starting before the
                            // final 64KiB. No PCM is decoded while measuring duration.
                            scan.start = scan.end.saturating_sub(2 * 65_307);
                            let start = scan.start;
                            callback(
                                e,
                                Callback::Seek,
                                scan.callbacks[1],
                                [scan.datasource, start as u32, (start >> 32) as u32, 0],
                                move |e, r| {
                                    if Provider::at(&e.sys.objects, scan.identity).is_none() {
                                        return total_error(&scan.name, OV_EINVAL);
                                    }
                                    if r.value() as i32 != 0 {
                                        return restore_scan(e, scan, Err(OV_EREAD));
                                    }
                                    scan_read(e, scan)
                                },
                            )
                        },
                    )
                },
            )
        },
    )
}

fn scan_read(e: &mut Env<'_>, scan: Scan) -> Outcome {
    if Provider::at(&e.sys.objects, scan.identity).is_none() {
        return total_error(&scan.name, OV_EINVAL);
    }
    let remaining = scan
        .end
        .saturating_sub(scan.start + scan.bytes.len() as u64);
    if remaining == 0 {
        let Some(s) = Provider::at(&e.sys.objects, scan.identity) else {
            return total_error(&scan.name, OV_EINVAL);
        };
        let serial = s.codec.format().serial;
        let total = adv_device::vorbis::tail_total(&scan.bytes, serial);
        return restore_scan(e, scan, total.filter(|&n| n != u64::MAX).ok_or(OV_EBADLINK));
    }
    let request = remaining.min(u64::from(READ_SIZE)) as u32;
    callback(
        e,
        Callback::Read,
        scan.callbacks[0],
        [scan.scratch, 1, request, scan.datasource],
        move |e, r| {
            let mut scan = scan;
            if Provider::at(&e.sys.objects, scan.identity).is_none() {
                return total_error(&scan.name, OV_EINVAL);
            }
            let length = r.value();
            if length == 0 || length > request {
                return restore_scan(e, scan, Err(OV_EREAD));
            }
            e.ext.get_mut::<Provider>().stats.callback_bytes += u64::from(length);
            let mut bytes = vec![0; length as usize];
            if e.cpu.read(scan.scratch, &mut bytes).is_err() {
                return restore_scan(e, scan, Err(OV_EREAD));
            }
            scan.bytes.extend(bytes);
            scan_read(e, scan)
        },
    )
}

fn restore_scan(e: &mut Env<'_>, scan: Scan, result: Result<u64, i32>) -> Outcome {
    if Provider::at(&e.sys.objects, scan.identity).is_none() {
        return total_error(&scan.name, OV_EINVAL);
    }
    callback(
        e,
        Callback::Seek,
        scan.callbacks[1],
        [
            scan.datasource,
            scan.saved as u32,
            (scan.saved >> 32) as u32,
            0,
        ],
        move |e, r| {
            let Some(s) = Provider::at_mut(&mut e.sys.objects, scan.identity) else {
                return total_error(&scan.name, OV_EINVAL);
            };
            s.busy = false;
            if r.value() as i32 != 0 {
                return total_error(&scan.name, OV_EREAD);
            }
            let total = match result {
                Ok(total) => total,
                Err(code) => return total_error(&scan.name, code),
            };
            s.codec.set_totals(total, scan.end);
            let outcome = if scan.name == "ov_time_total" {
                Outcome::ReturnF64(total as f64 / f64::from(s.codec.format().sample_rate))
            } else if scan.name == "ov_pcm_total" {
                Outcome::Return64(total)
            } else {
                Outcome::Return64(scan.end)
            };
            publish(e, scan.vf);
            outcome
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compatible_ordinals(bytes: &[u8]) -> bool {
        ordinal_layout_error(bytes).is_none()
    }
    fn unsupported_dynamic_names(bytes: &[u8]) -> bool {
        struct Empty;
        impl FileSource for Empty {
            fn read(&self, _: &str) -> Option<Vec<u8>> {
                None
            }
        }
        unsupported_dynamic_symbol(bytes, &lower_symbols(&Empty)).is_some()
    }

    fn export_fixture() -> Vec<u8> {
        // A small sectionless PE32 with the headers mapped as its whole image.
        let mut bytes = vec![0u8; 4096];
        fn u16_at(bytes: &mut [u8], at: usize, value: u16) {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn u32_at(bytes: &mut [u8], at: usize, value: u32) {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[..2].copy_from_slice(b"MZ");
        u32_at(&mut bytes, 0x3c, 0x80);
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        u16_at(&mut bytes, 0x84, 0x14c);
        u16_at(&mut bytes, 0x94, 224);
        u16_at(&mut bytes, 0x98, 0x10b);
        u32_at(&mut bytes, 0x98 + 56, 4096);
        u32_at(&mut bytes, 0x98 + 60, 4096);
        u32_at(&mut bytes, 0x98 + 92, 16);
        u32_at(&mut bytes, 0x98 + 96, 0x200);
        u32_at(&mut bytes, 0x98 + 100, 0x600);
        u32_at(&mut bytes, 0x210, 1);
        u32_at(&mut bytes, 0x214, 25);
        u32_at(&mut bytes, 0x218, EXPORTS.len() as u32);
        u32_at(&mut bytes, 0x21c, 0x240);
        u32_at(&mut bytes, 0x220, 0x2c0);
        u32_at(&mut bytes, 0x224, 0x310);
        let mut string = 0x360;
        for (i, &(name, _, ordinal)) in EXPORTS.iter().enumerate() {
            u32_at(&mut bytes, 0x240 + (ordinal as usize - 1) * 4, 0x900);
            u32_at(&mut bytes, 0x2c0 + i * 4, string as u32);
            u16_at(&mut bytes, 0x310 + i * 2, ordinal - 1);
            bytes[string..string + name.len()].copy_from_slice(name.as_bytes());
            string += name.len() + 1;
        }
        bytes
    }

    #[test]
    fn actual_dll_ordinal_names_must_match_before_host_selection() {
        struct Files(Vec<u8>);
        impl FileSource for Files {
            fn read(&self, path: &str) -> Option<Vec<u8>> {
                matches!(path, "app.exe" | "vorbisfile.dll").then(|| self.0.clone())
            }
        }
        let mut bytes = export_fixture();
        assert!(compatible_ordinals(&bytes));
        assert_eq!(
            select_policy(&Files(bytes.clone()), "app.exe", Policy::Native).unwrap(),
            Policy::Native
        );
        // Identical names with a different export ordinal base are not the
        // same ABI for static ordinal imports or GetProcAddress ordinals.
        bytes[0x210..0x214].copy_from_slice(&2u32.to_le_bytes());
        assert!(!compatible_ordinals(&bytes));
        assert_eq!(
            select_policy(&Files(bytes), "app.exe", Policy::Native).unwrap(),
            Policy::Guest
        );
    }

    #[test]
    fn uninspectable_pe_or_imports_choose_whole_guest_codec_with_path_and_stage() {
        struct Files(Vec<u8>);
        impl FileSource for Files {
            fn read(&self, path: &str) -> Option<Vec<u8>> {
                (path == "packed.exe").then(|| self.0.clone())
            }
        }
        let put = |bytes: &mut [u8], at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        for stage in ["PE headers/image", "imports", "delay imports"] {
            let mut bytes = export_fixture();
            match stage {
                "PE headers/image" => bytes[..2].copy_from_slice(b"xx"),
                "imports" => {
                    put(&mut bytes, 0x98 + 96 + 8, 0x800);
                    put(&mut bytes, 0x98 + 100 + 8, 40);
                    put(&mut bytes, 0x800, 0xffff_fffc);
                    put(&mut bytes, 0x80c, 0x950);
                    put(&mut bytes, 0x810, 0x920);
                    bytes[0x950..0x95d].copy_from_slice(b"kernel32.dll\0");
                }
                _ => {
                    put(&mut bytes, 0x98 + 96 + 13 * 8, 0x800);
                    put(&mut bytes, 0x98 + 100 + 13 * 8, 64);
                    put(&mut bytes, 0x800, 1);
                    put(&mut bytes, 0x804, 0x950);
                    put(&mut bytes, 0x810, 0xffff_fffc);
                    bytes[0x950..0x95d].copy_from_slice(b"kernel32.dll\0");
                }
            }
            let selected =
                select_policy_with_reason(&Files(bytes), "packed.exe", Policy::Native).unwrap();
            assert_eq!(selected.policy, Policy::Guest);
            let reason = selected.reason.unwrap();
            assert!(
                reason.contains("packed.exe") && reason.contains(stage),
                "{reason}"
            );
        }
    }

    #[test]
    fn capability_scan_covers_dynamic_names_without_rejecting_supported_exports() {
        assert!(!unsupported_dynamic_names(
            b"\0ov_read\0ov_open_callbacks\0ov_info\0"
        ));
        for bytes in [
            b"\0ov_raw_seek\0".as_slice(),
            b"\0ov_read_float\0",
            b"\0vorbis_synthesis\0",
            b"\0ogg_stream_init\0",
            b"\0vorbis.dll\0",
        ] {
            assert!(unsupported_dynamic_names(bytes));
        }
        assert!(!unsupported_dynamic_names(
            b"\0ov_not_a_symbol has spaces\0"
        ));
        assert!(!unsupported_dynamic_names(
            b"\0ogg_intro_src\0ogg_loop_src\0vorbis_info\0"
        ));
    }

    #[test]
    fn comments_have_32bit_pointers_lengths_and_terminated_strings() {
        let mut packet = b"\x03vorbis".to_vec();
        packet.extend_from_slice(&3u32.to_le_bytes());
        packet.extend_from_slice(b"abc");
        packet.extend_from_slice(&2u32.to_le_bytes());
        for value in [b"TITLE=tone".as_slice(), b"ARTIST=fixture"] {
            packet.extend_from_slice(&(value.len() as u32).to_le_bytes());
            packet.extend_from_slice(value);
        }
        packet.push(1);
        let base = 0x1000u32;
        let bytes = parse_comments(&packet, base).unwrap();
        let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(word(8), 2);
        let pointers = (word(0) - base) as usize;
        let lengths = (word(4) - base) as usize;
        let vendor = (word(12) - base) as usize;
        assert_eq!(&bytes[vendor..vendor + 4], b"abc\0");
        for (i, value) in [b"TITLE=tone".as_slice(), b"ARTIST=fixture"]
            .iter()
            .enumerate()
        {
            let string = (word(pointers + i * 4) - base) as usize;
            assert_eq!(word(lengths + i * 4), value.len() as u32);
            assert_eq!(&bytes[string..string + value.len()], *value);
            assert_eq!(bytes[string + value.len()], 0);
        }
        assert_eq!(word(pointers + 8), 0);
        packet.pop();
        assert_eq!(parse_comments(&packet, base), Err(OV_EBADHEADER));
    }
}
