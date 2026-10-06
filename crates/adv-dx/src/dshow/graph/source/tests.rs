use super::*;
use crate::dshow::tests::{activate, env, machine};
#[cfg(feature = "media")]
use crate::dshow::tests::{method, query};
use adv_cpu::backend::{Guest, Reg};
use adv_machine::{Machine, Resume};

const DATA: u32 = 0x500000;
const STACK: u32 = 0x600000;

pub(crate) fn finish(m: &mut Machine<adv_cpu::JitCpu>, outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        Outcome::Call {
            target, args, then, ..
        } => {
            let index =
                (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let handler = m.sys.thunks.get(index).f.handler;
            let old_stack = m.cpu.reg(Reg::Esp);
            m.cpu.set_reg(Reg::Esp, STACK);
            for (i, word) in args.iter().enumerate() {
                m.cpu.write_u32(STACK + 4 + i as u32 * 4, *word).unwrap();
            }
            let mut context = env(m);
            context.thunk = index;
            let result = handler(&mut context);
            let result = finish(m, result);
            m.cpu.set_reg(Reg::Esp, old_stack);
            let result = then(&mut env(m), Resume::Returned(result));
            finish(m, result)
        }
        _ => panic!("file opening must be synchronous"),
    }
}
fn wide(m: &mut Machine<adv_cpu::JitCpu>, at: u32, text: &str) {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(at, &bytes).unwrap();
}

#[test]
fn file_source_validates_outputs_and_does_not_publish_failed_opens() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    for (filename, output) in [(0, DATA), (DATA + 256, 0), (0xffff0000, DATA)] {
        m.cpu.write_u32(DATA, u32::MAX).unwrap();
        let outcome = add(&mut env(&mut m), graph, filename, 0, output);
        assert_eq!(finish(&mut m, outcome), E_POINTER);
        assert!(m.ext.get::<State>().graphs[&graph].members.is_empty());
    }
    wide(&mut m, DATA + 256, "target/nonexistent-dshow-source.wav");
    let outcome = add(&mut env(&mut m), graph, DATA + 256, 0, DATA);
    let hr = finish(&mut m, outcome);
    assert_eq!(
        hr,
        if Decoder::available() {
            VFW_E_NOT_FOUND
        } else {
            VFW_E_UNSUPPORTED_STREAM
        }
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
    let directory = std::path::Path::new("target/dshow-source-tests");
    std::fs::create_dir_all(directory).unwrap();
    let filename = format!("invalid-{}.bin", std::process::id());
    std::fs::write(directory.join(&filename), [0x55; 32]).unwrap();
    wide(
        &mut m,
        DATA + 256,
        &format!("target/dshow-source-tests/{filename}"),
    );
    let outcome = add(&mut env(&mut m), graph, DATA + 256, 0, DATA);
    assert_eq!(finish(&mut m, outcome), VFW_E_UNSUPPORTED_STREAM);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}

#[cfg(feature = "media")]
fn authored_wave() -> String {
    let root = std::path::Path::new("target/dshow-source-tests");
    std::fs::create_dir_all(root).unwrap();
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let filename = format!(
        "authored-{}-{}.wav",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let path = root.join(&filename);
    let samples = 800u32;
    let size = samples * 2;
    let mut wave = Vec::new();
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(36 + size).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&8000u32.to_le_bytes());
    wave.extend_from_slice(&16000u32.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes());
    wave.extend_from_slice(&16u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&size.to_le_bytes());
    wave.resize(wave.len() + size as usize, 0);
    std::fs::write(path, wave).unwrap();
    format!(
        "TARGET\\DSHOW-SOURCE-TESTS\\{}",
        filename.to_ascii_uppercase()
    )
}

#[cfg(feature = "media")]
#[test]
fn file_source_uses_guest_paths_keeps_decoder_and_tracks_graph_and_caller_references() {
    let guest_path = authored_wave();
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    wide(&mut m, DATA + 256, &guest_path);
    wide(&mut m, DATA + 1024, "Authored source");
    let outcome = add(&mut env(&mut m), graph, DATA + 256, DATA + 1024, DATA);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let source = m.cpu.read_u32(DATA).unwrap();
    assert_ne!(source, 0);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 1);
    let write_open = m.resolve_import("kernel32.dll", "CreateFileW").unwrap();
    let write_args = vec![DATA + 256, 0x4000_0000, 7, 0, 3, 0x80, 0];
    let outcome = Outcome::call(write_open, write_args.clone(), |_, resume| {
        Outcome::Return(resume.value())
    });
    assert_eq!(finish(&mut m, outcome), u32::MAX); // The source owns the read-only sharing lock.
    let source_ref = &m.ext.get::<State>().filters[&source];
    assert_eq!(
        source_ref.name,
        "Authored source".encode_utf16().collect::<Vec<_>>()
    );
    assert!(source_ref.source.as_ref().unwrap().decoder.is_some());
    let file_face = query(&mut m, source, IID_IFILESOURCEFILTER);
    assert_eq!(
        method(
            &mut m,
            "IFileSourceFilter::GetCurFile",
            &[file_face, DATA + 4, DATA + 64]
        ),
        S_OK
    );
    let text = m.cpu.read_u32(DATA + 4).unwrap();
    assert_eq!(
        read_wide(&env(&mut m), text, 32768),
        guest_path.encode_utf16().collect::<Vec<_>>()
    );
    adv_os::com::task_free(&mut env(&mut m), text);
    assert_eq!(
        method(&mut m, "IFileSourceFilter::Release", &[file_face]),
        2
    );
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[source]), 1);
    assert_eq!(
        method(&mut m, "IFilterGraph2::RemoveFilter", &[graph, source]),
        S_OK
    );
    assert!(!m.ext.get::<State>().filters.contains_key(&source));
    assert!(m.ext.get::<State>().graphs[&graph].members.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
    wide(&mut m, DATA + 256, &guest_path); // query() used this scratch area for the IID.
    let outcome = Outcome::call(write_open, write_args, |_, resume| {
        Outcome::Return(resume.value())
    });
    let write_handle = finish(&mut m, outcome);
    assert_ne!(write_handle, u32::MAX);
    assert!(m.sys.objects.close(write_handle).is_some());
}

#[cfg(feature = "media")]
#[test]
fn automatic_audio_graph_owns_the_file_and_negotiates_pcm_without_a_video_renderer() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let guest_path = authored_wave();
    wide(&mut m, DATA + 256, &guest_path);
    let outcome = add(&mut env(&mut m), graph, DATA + 256, 0, DATA);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let source = m.cpu.read_u32(DATA).unwrap();
    let output = m.ext.get::<State>().filters[&source].pins[0];
    let outcome = super::super::render::pin(&mut env(&mut m), graph, output);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 4);
    let splitter = state
        .filters
        .iter()
        .find(|(_, f)| f.role == Role::Splitter)
        .unwrap()
        .0;
    assert!(state.filters[splitter]
        .source
        .as_ref()
        .unwrap()
        .decoder
        .is_some());
    let renderer = state
        .filters
        .values()
        .find(|f| f.role == Role::AudioRenderer)
        .unwrap();
    let input = &state.pins[&renderer.pins[0]];
    assert_ne!(input.peer, 0);
    assert_eq!(input.mt.as_ref().unwrap().sub, MEDIASUBTYPE_PCM);
    assert_eq!(duration(&mut env(&mut m), graph), 1_000_000);
    let outcome = super::super::render::pin(&mut env(&mut m), graph, output);
    assert_eq!(finish(&mut m, outcome), VFW_E_ALREADY_CONNECTED);
    let file_face = query(&mut m, source, IID_IFILESOURCEFILTER);
    assert_eq!(
        method(
            &mut m,
            "IFileSourceFilter::Load",
            &[file_face, DATA + 256, 0]
        ),
        VFW_E_WRONG_STATE
    );
    method(&mut m, "IFileSourceFilter::Release", &[file_face]);
    method(&mut m, "IBaseFilter::Release", &[source]);
    assert_eq!(method(&mut m, "IFilterGraph2::Release", &[graph]), 0);
    assert!(m.ext.get::<State>().graphs.is_empty());
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}
