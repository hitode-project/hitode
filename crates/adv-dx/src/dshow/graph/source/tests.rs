use super::*;
use crate::dshow::tests::{activate, env, machine, method, query};
use adv_cpu::backend::{Guest, Reg};
use adv_machine::{Machine, Resume};
use adv_os::vfs::host;
use std::path::Path;

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
// spec: MEDIA-GRAPH-2
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
    let directory = Path::new("./target/dshow-source-tests");
    host::create_dir_all(directory).unwrap();
    let filename = format!("invalid-{}.bin", std::process::id());
    host::write(&directory.join(&filename), &[0x55; 32]).unwrap();
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
    authored("wav", &wave())
}

/// Write `bytes` under the test root and return its guest path.
#[cfg(feature = "media")]
fn authored(extension: &str, bytes: &[u8]) -> String {
    let root = Path::new("./target/dshow-source-tests");
    host::create_dir_all(root).unwrap();
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let filename = format!(
        "authored-{}-{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let path = root.join(&filename);
    host::write(&path, bytes).unwrap();
    format!(
        "TARGET\\DSHOW-SOURCE-TESTS\\{}",
        filename.to_ascii_uppercase()
    )
}

#[cfg(feature = "media")]
fn wave() -> Vec<u8> {
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
    wave
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-2
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
    assert!(
        state.filters[splitter]
            .source
            .as_ref()
            .unwrap()
            .decoder
            .is_some()
    );
    let renderer = state
        .filters
        .values()
        .find(|f| f.role == Role::AudioRenderer)
        .unwrap();
    let input = &state.pins[&renderer.pins[0]];
    assert_ne!(input.peer, 0);
    assert_eq!(input.mt.as_ref().unwrap().sub, MEDIASUBTYPE_PCM);
    assert_eq!(duration(&env(&mut m), graph), 1_000_000);
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

#[cfg(feature = "media")]
fn invoke(
    m: &mut Machine<adv_cpu::JitCpu>,
    pointer: u32,
    interface: &str,
    name: &str,
    args: &[u32],
) -> u32 {
    let slot = adv_abi::signature::com_signatures()
        .iter()
        .find(|s| s.module == interface && s.name == name)
        .unwrap()
        .com_slot
        .unwrap();
    let outcome = guest_call(
        &mut env(m),
        pointer,
        u32::from(slot),
        args.to_vec(),
        |_, hr| Outcome::Return(hr),
    );
    finish(m, outcome)
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-4
fn render_file_appends_through_both_interfaces_and_owns_only_graph_references() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IFILTERGRAPH2);
    let control = query(&mut m, graph, IID_IMEDIACONTROL);
    let path = authored_wave();
    wide(&mut m, DATA + 1024, &path);
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "RenderFile", &[0, 0]),
        E_POINTER
    );
    assert!(m.ext.get::<State>().filters.is_empty());
    // Native ignores the reserved playlist pointer rather than dereferencing it.
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "RenderFile",
            &[DATA + 1024, u32::MAX]
        ),
        S_OK
    );
    let first: Vec<_> = m.ext.get::<State>().graphs[&graph]
        .members
        .iter()
        .map(|member| member.filter)
        .collect();
    assert_eq!(first.len(), 4);
    assert_eq!(
        invoke(
            &mut m,
            control,
            "IMediaControl",
            "RenderFile",
            &[DATA + 1024]
        ),
        S_OK
    );
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 8);
    assert!(first.iter().all(|f| state.filters.contains_key(f)));
    assert_eq!(
        state
            .filters
            .values()
            .filter(|f| f.role == Role::AudioRenderer)
            .count(),
        2
    );
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 2);
    wide(&mut m, DATA + 1024, "target/nonexistent-dshow-source.wav");
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "RenderFile",
            &[DATA + 1024, 0]
        ),
        VFW_E_NOT_FOUND
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 8);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 2);
    assert_eq!(method(&mut m, "IMediaControl::Release", &[control]), 1);
    assert_eq!(invoke(&mut m, graph, "IFilterGraph2", "Release", &[]), 0);
    assert!(m.ext.get::<State>().graphs.is_empty());
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-5
fn render_ex_existing_only_rolls_back_then_retries_with_existing_audio_renderer() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IFILTERGRAPH2);
    wide(&mut m, DATA + 1024, &authored_wave());
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "AddSourceFilter",
            &[DATA + 1024, 0, DATA]
        ),
        S_OK
    );
    let source = m.cpu.read_u32(DATA).unwrap();
    let output = m.ext.get::<State>().filters[&source].pins[0];
    for args in [[output, 2, 0], [output, 0, 1]] {
        assert_eq!(
            invoke(&mut m, graph, "IFilterGraph2", "RenderEx", &args),
            E_INVALIDARG
        );
    }
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "RenderEx", &[output, 1, 0]),
        0x8004_0266
    );
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 1);
    assert_eq!(state.pins[&output].peer, 0);
    assert!(
        state.filters[&source]
            .source
            .as_ref()
            .unwrap()
            .decoder
            .is_some()
    );
    let renderer = activate(&mut m, CLSID_DSOUND_RENDER, IID_IBASEFILTER);
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "AddFilter", &[renderer, 0]),
        S_OK
    );
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "RenderEx", &[output, 1, 0]),
        S_OK
    );
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 4);
    assert_eq!(
        state
            .filters
            .values()
            .filter(|f| f.role == Role::AudioRenderer)
            .count(),
        1
    );
    assert_ne!(state.pins[&state.filters[&renderer].pins[0]].peer, 0);
    method(&mut m, "IBaseFilter::Release", &[renderer]);
    method(&mut m, "IBaseFilter::Release", &[source]);
    assert_eq!(invoke(&mut m, graph, "IFilterGraph2", "Release", &[]), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-4
fn render_file_movie_keeps_connected_renderers_and_appends_video_and_audio_streams() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IFILTERGRAPH2);
    crate::dshow::video::set_video_provider(
        &mut m.ext,
        VideoProvider {
            window: |_, _, _| None,
            update: |_, _, _, _| Ok(1),
            completed: |_, _| None,
        },
    );
    let path = format!(
        "./target/dshow-source-tests/movie-{}.mpg",
        std::process::id()
    );
    host::create_dir_all(Path::new("./target/dshow-source-tests")).unwrap();
    host::write(
        Path::new(&path),
        include_bytes!("../render/fixtures/guest_movie.mpg"),
    )
    .unwrap();
    wide(&mut m, DATA + 1024, &path);
    for count in [6, 12] {
        assert_eq!(
            invoke(
                &mut m,
                graph,
                "IFilterGraph2",
                "RenderFile",
                &[DATA + 1024, 0]
            ),
            S_OK
        );
        let state = m.ext.get::<State>();
        assert_eq!(state.graphs[&graph].members.len(), count);
        for f in state
            .filters
            .values()
            .filter(|f| matches!(f.role, Role::AudioRenderer | Role::VideoRenderer))
        {
            assert_ne!(state.pins[&f.pins[0]].peer, 0);
        }
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 2);
    assert_eq!(invoke(&mut m, graph, "IFilterGraph2", "Release", &[]), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-5
fn render_ex_reuses_video_and_audio_renderers_and_normal_mode_creates_new_ones_when_busy() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IFILTERGRAPH2);
    crate::dshow::video::set_video_provider(
        &mut m.ext,
        VideoProvider {
            window: |_, _, _| None,
            update: |_, _, _, _| Ok(1),
            completed: |_, _| None,
        },
    );
    let path = format!(
        "./target/dshow-source-tests/movie-existing-{}.mpg",
        std::process::id()
    );
    host::create_dir_all(Path::new("./target/dshow-source-tests")).unwrap();
    host::write(
        Path::new(&path),
        include_bytes!("../render/fixtures/guest_movie.mpg"),
    )
    .unwrap();
    wide(&mut m, DATA + 1024, &path);
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "AddSourceFilter",
            &[DATA + 1024, 0, DATA]
        ),
        S_OK
    );
    let source = m.cpu.read_u32(DATA).unwrap();
    let output = m.ext.get::<State>().filters[&source].pins[0];
    for class in [CLSID_VIDEO_RENDER, CLSID_DSOUND_RENDER] {
        let role = if class == CLSID_VIDEO_RENDER {
            Role::VideoRenderer
        } else {
            Role::AudioRenderer
        };
        let renderer = filter::new_filter(&mut env(&mut m), role, class).unwrap();
        assert_eq!(
            invoke(&mut m, graph, "IFilterGraph2", "AddFilter", &[renderer, 0]),
            S_OK
        );
        method(&mut m, "IBaseFilter::Release", &[renderer]);
    }
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "RenderEx", &[output, 1, 0]),
        S_OK
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 6);
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "AddSourceFilter",
            &[DATA + 1024, 0, DATA]
        ),
        S_OK
    );
    let second_source = m.cpu.read_u32(DATA).unwrap();
    let second_output = m.ext.get::<State>().filters[&second_source].pins[0];
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "RenderEx",
            &[second_output, 1, 0]
        ),
        0x8004_0266
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 7);
    assert_eq!(
        invoke(
            &mut m,
            graph,
            "IFilterGraph2",
            "RenderEx",
            &[second_output, 0, 0]
        ),
        S_OK
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 12);
    for role in [Role::VideoRenderer, Role::AudioRenderer] {
        assert_eq!(
            m.ext
                .get::<State>()
                .filters
                .values()
                .filter(|f| f.role == role)
                .count(),
            2
        );
    }
    method(&mut m, "IBaseFilter::Release", &[source]);
    method(&mut m, "IBaseFilter::Release", &[second_source]);
    assert_eq!(invoke(&mut m, graph, "IFilterGraph2", "Release", &[]), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}

#[test]
// spec: MEDIA-GRAPH-2
fn asf_reader_is_a_native_file_source_until_loaded() {
    let mut m = machine();
    assert!(has_graph_class(&CLSID_WMASF_READER));
    let reader = activate(&mut m, CLSID_WMASF_READER, IID_IBASEFILTER);
    assert_eq!(
        method(&mut m, "IBaseFilter::GetClassID", &[reader, DATA + 64]),
        S_OK
    );
    let mut class = [0; 16];
    m.cpu.read(DATA + 64, &mut class).unwrap();
    assert_eq!(class, CLSID_WMASF_READER);
    let state = m.ext.get::<State>();
    assert_eq!(state.filters[&reader].role, Role::FileSource);
    assert!(state.filters[&reader].source_path.is_empty());
    let file_face = query(&mut m, reader, IID_IFILESOURCEFILTER);
    method(&mut m, "IFileSourceFilter::Release", &[file_face]);
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[reader]), 0);
    assert!(m.ext.get::<State>().filters.is_empty());
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-GRAPH-2
fn asf_reader_load_probes_windows_media_video_and_audio() {
    let mut m = machine();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    // ffmpeg -f lavfi -i testsrc=size=32x16:rate=10 -f lavfi -i sine=frequency=440:sample_rate=22050
    // -t 0.3 -c:v wmv2 -b:v 50k -c:a wmav2 -b:a 32k -ac 1 -f asf asf_movie.wmv
    let guest_path = authored("wmv", include_bytes!("fixtures/asf_movie.wmv"));
    // query() stages its IID at DATA + 256.
    wide(&mut m, DATA + 512, &guest_path);
    let reader = activate(&mut m, CLSID_WMASF_READER, IID_IBASEFILTER);
    let file_face = query(&mut m, reader, IID_IFILESOURCEFILTER);
    assert_eq!(
        invoke(
            &mut m,
            file_face,
            "IFileSourceFilter",
            "Load",
            &[DATA + 512, 0]
        ),
        S_OK
    );
    let info = m.ext.get::<State>().filters[&reader]
        .source
        .as_ref()
        .and_then(|s| s.info.clone())
        .unwrap();
    let video = info.video.unwrap();
    assert_eq!((video.width, video.height), (32, 16));
    assert_eq!(info.audio.unwrap().sample_rate, 22050);
    method(&mut m, "IFileSourceFilter::Release", &[file_face]);
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[reader]), 0);
    assert!(m.sys.objects.hosts::<FileObj>().next().is_none());
}
