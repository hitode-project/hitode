use std::sync::Arc;

use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::Machine;

use super::*;
use crate::dshow::{CLSID_FILTERGRAPH, IID_IGRAPHBUILDER, tests};

const DATA: u32 = 0x500000;
const STACK: u32 = 0x600000;
const READ_ONLY: u32 = 0x700000;

fn info() -> VideoInfo {
    VideoInfo {
        codec: "raw".into(),
        width: 3,
        height: 2,
        frame_rate: (30_000, 1001),
    }
}

fn args(m: &mut Machine<JitCpu>, args: &[u32]) {
    m.cpu.set_reg(Reg::Esp, STACK);
    for (index, &value) in args.iter().enumerate() {
        m.cpu
            .write_u32(STACK + 4 + index as u32 * 4, value)
            .unwrap();
    }
}

fn graph(m: &mut Machine<JitCpu>) -> u32 {
    tests::activate(m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER)
}

fn provider(m: &mut Machine<JitCpu>, fail: bool) {
    m.ext.insert(crate::dshow::video::VideoProvider {
        window: |_, _, owner| {
            (owner == 42).then_some(crate::dshow::video::VideoWindowGeometry {
                width: 960,
                height: 544,
                origin: (0, 0),
                canvas: (960, 544),
                visible: true,
            })
        },
        update: if fail {
            |_, _, _, _| Err("test presentation failure".into())
        } else {
            |_, _, _, _| Ok(0)
        },
        completed: |_, _| None,
    });
}

#[test]
// spec: MEDIA-VIDEO-3
fn empty_graph_keeps_video_distributor_no_interface_contract() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    for (face, method) in [
        (Face::VideoWindow, "put_Owner"),
        (Face::BasicVideo, "GetVideoSize"),
    ] {
        args(&mut m, &[graph, 0, 0]);
        assert!(matches!(
            call(&mut tests::env(&mut m), graph, face, method),
            Some(Outcome::Return(E_NOINTERFACE))
        ));
    }
}

#[test]
// spec: MEDIA-VIDEO-2
fn position_getters_validate_every_output_before_writing_any() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    let expected = [0xcccc_cccc; 4];
    for bad in [0, READ_ONLY, DATA + 4094, u32::MAX - 1] {
        for method in ["GetWindowPosition", "GetRestorePosition"] {
            for slot in 0..4 {
                for (i, &value) in expected.iter().enumerate() {
                    m.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
                }
                let mut output = [graph, DATA, DATA + 4, DATA + 8, DATA + 12];
                output[slot + 1] = bad;
                args(&mut m, &output);
                assert_eq!(
                    window_call(&mut tests::env(&mut m), graph, method, &info()),
                    E_POINTER
                );
                for (i, &value) in expected.iter().enumerate() {
                    assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), value);
                }
            }
        }
        args(&mut m, &[graph, DATA, bad]);
        assert_eq!(
            basic_call(&mut tests::env(&mut m), graph, "GetVideoSize", &info()),
            E_POINTER
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), expected[0]);
    }
}

#[test]
// spec: MEDIA-VIDEO-2
fn double_outputs_require_all_eight_bytes_and_return_actual_frame_time() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    for bad in [0, DATA + 4092, u32::MAX - 3] {
        args(&mut m, &[graph, bad]);
        assert_eq!(
            basic_call(
                &mut tests::env(&mut m),
                graph,
                "get_AvgTimePerFrame",
                &info()
            ),
            E_POINTER
        );
    }
    args(&mut m, &[graph, DATA]);
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "get_AvgTimePerFrame",
            &info()
        ),
        S_OK
    );
    let mut bytes = [0; 8];
    m.cpu.read(DATA, &mut bytes).unwrap();
    assert_eq!(f64::from_le_bytes(bytes), 1001.0 / 30_000.0);
}

#[test]
// spec: MEDIA-VIDEO-1
fn invalid_rectangles_and_automation_booleans_do_not_change_state() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    let before = m.ext.get::<State>().graphs[&graph].video.clone();
    for rect in [
        [-1, 0, 1, 1],
        [0, 0, 0, 1],
        [0, 0, 1, -1],
        [2, 0, 2, 1],
        [0, 1, 1, 2],
        [i32::MAX, 0, 1, 1],
    ] {
        args(
            &mut m,
            &[
                graph,
                rect[0] as u32,
                rect[1] as u32,
                rect[2] as u32,
                rect[3] as u32,
            ],
        );
        assert_eq!(
            basic_call(&mut tests::env(&mut m), graph, "SetSourcePosition", &info()),
            E_INVALIDARG
        );
        assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
    }
    for method in ["put_AutoShow", "put_Visible", "HideCursor"] {
        for value in [1, 2, 0x8000_0000] {
            args(&mut m, &[graph, value]);
            assert_eq!(
                window_call(&mut tests::env(&mut m), graph, method, &info()),
                E_INVALIDARG
            );
            assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
        }
    }
    for rect in [
        [0, 0, 0, 1],
        [0, 0, 1, -1],
        [i32::MAX, 0, 1, 1],
        [0, i32::MAX, 1, 1],
    ] {
        args(
            &mut m,
            &[
                graph,
                rect[0] as u32,
                rect[1] as u32,
                rect[2] as u32,
                rect[3] as u32,
            ],
        );
        assert_eq!(
            window_call(&mut tests::env(&mut m), graph, "SetWindowPosition", &info()),
            E_INVALIDARG
        );
        assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
    }
}

#[test]
// spec: MEDIA-VIDEO-1
fn embedded_window_sequence_preserves_owner_geometry_visibility_and_cursor() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    provider(&mut m, false);
    for (method, values) in [
        ("put_Owner", vec![42]),
        ("put_WindowStyle", vec![0x4600_0000]),
        ("SetWindowPosition", vec![0, 0, 960, 544]),
        ("put_Visible", vec![u32::MAX]),
        ("HideCursor", vec![u32::MAX]),
    ] {
        let mut all = vec![graph];
        all.extend(values);
        args(&mut m, &all);
        assert_eq!(
            window_call(&mut tests::env(&mut m), graph, method, &info()),
            S_OK
        );
    }
    let state = &m.ext.get::<State>().graphs[&graph].video;
    assert_eq!(state.owner, 42);
    assert_eq!(state.style, 0x5600_0000);
    assert_eq!(
        state.effective_position(&info()),
        Rect {
            left: 0,
            top: 0,
            width: 960,
            height: 544
        }
    );
    assert_eq!(
        state.effective_destination(&info()),
        state.effective_position(&info())
    );
    assert_eq!(state.effective_source(&info()), Rect::image(&info()));
    assert!(state.visible && state.auto_show && state.hide_cursor);
    for method in ["get_Visible", "get_AutoShow", "IsCursorHidden"] {
        args(&mut m, &[graph, DATA]);
        assert_eq!(
            window_call(&mut tests::env(&mut m), graph, method, &info()),
            S_OK
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::MAX);
    }
    args(&mut m, &[graph, DATA]);
    assert_eq!(
        window_call(&mut tests::env(&mut m), graph, "get_WindowState", &info()),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), SW_SHOW);
    for method in ["put_Visible", "HideCursor", "put_AutoShow", "put_Owner"] {
        args(&mut m, &[graph, 0]);
        assert_eq!(
            window_call(&mut tests::env(&mut m), graph, method, &info()),
            S_OK
        );
    }
    let state = &m.ext.get::<State>().graphs[&graph].video;
    assert_eq!(state.owner, 0);
    assert!(!state.visible && !state.auto_show && !state.hide_cursor);
    assert_eq!(state.style & WS_VISIBLE, 0);
}

#[test]
// spec: MEDIA-VIDEO-1
fn invalid_owner_and_failed_presentation_leave_window_state_unchanged() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    provider(&mut m, false);
    let before = m.ext.get::<State>().graphs[&graph].video.clone();
    args(&mut m, &[graph, 43]);
    assert_eq!(
        window_call(&mut tests::env(&mut m), graph, "put_Owner", &info()),
        E_INVALIDARG
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
    provider(&mut m, true);
    args(&mut m, &[graph, 0, 0, 960, 544]);
    assert_eq!(
        window_call(&mut tests::env(&mut m), graph, "SetWindowPosition", &info()),
        E_FAIL
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
}

#[test]
// spec: MEDIA-VIDEO-2
fn source_and_destination_are_independent_and_default_destination_follows_resize() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    provider(&mut m, false);
    args(&mut m, &[graph, 1, 0, 2, 2]);
    assert_eq!(
        basic_call(&mut tests::env(&mut m), graph, "SetSourcePosition", &info()),
        S_OK
    );
    args(&mut m, &[graph, (-5i32) as u32, (-3i32) as u32, 50, 30]);
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "SetDestinationPosition",
            &info()
        ),
        S_OK
    );
    for method in ["IsUsingDefaultSource", "IsUsingDefaultDestination"] {
        assert_eq!(
            basic_call(&mut tests::env(&mut m), graph, method, &info()),
            S_FALSE
        );
    }
    args(&mut m, &[graph, DATA, DATA + 4, DATA + 8, DATA + 12]);
    assert_eq!(
        basic_call(&mut tests::env(&mut m), graph, "GetSourcePosition", &info()),
        S_OK
    );
    for (i, value) in [1, 0, 2, 2].into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), value);
    }
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "GetDestinationPosition",
            &info()
        ),
        S_OK
    );
    for (i, value) in [(-5i32) as u32, (-3i32) as u32, 50, 30]
        .into_iter()
        .enumerate()
    {
        assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), value);
    }
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "SetDefaultDestinationPosition",
            &info()
        ),
        S_OK
    );
    args(&mut m, &[graph, 0, 0, 960, 544]);
    assert_eq!(
        window_call(&mut tests::env(&mut m), graph, "SetWindowPosition", &info()),
        S_OK
    );
    args(&mut m, &[graph, DATA, DATA + 4, DATA + 8, DATA + 12]);
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "GetDestinationPosition",
            &info()
        ),
        S_OK
    );
    for (i, value) in [0, 0, 960, 544].into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), value);
    }
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "IsUsingDefaultSource",
            &info()
        ),
        S_FALSE
    );
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "IsUsingDefaultDestination",
            &info()
        ),
        S_OK
    );
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "SetDefaultSourcePosition",
            &info()
        ),
        S_OK
    );
    assert_eq!(
        basic_call(
            &mut tests::env(&mut m),
            graph,
            "IsUsingDefaultSource",
            &info()
        ),
        S_OK
    );
    assert_eq!(
        m.ext.get::<State>().graphs[&graph]
            .video
            .effective_source(&info()),
        Rect::image(&info())
    );
}

#[test]
// spec: MEDIA-VIDEO-6
fn unsupported_window_modes_report_failure_and_preserve_geometry() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    let before = m.ext.get::<State>().graphs[&graph].video.clone();
    for (method, value) in [
        ("put_FullScreenMode", u32::MAX),
        ("put_WindowState", 3),
        ("put_WindowStyleEx", 8),
        ("put_MessageDrain", 1),
        ("put_Visible", u32::MAX),
        ("put_WindowState", SW_SHOW),
        ("put_WindowStyle", WS_VISIBLE),
    ] {
        args(&mut m, &[graph, value]);
        assert_eq!(
            window_call(&mut tests::env(&mut m), graph, method, &info()),
            E_NOTIMPL
        );
        assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
    }
    args(&mut m, &[graph, 0x2000_0000]);
    assert_eq!(
        window_call(&mut tests::env(&mut m), graph, "put_WindowStyle", &info()),
        E_INVALIDARG
    );
    assert_eq!(m.ext.get::<State>().graphs[&graph].video, before);
    assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
// spec: MEDIA-VIDEO-3
fn snapshot_crops_source_and_writes_bottom_up_rows_with_checked_capacity() {
    let mut m = tests::machine();
    let graph = graph(&mut m);
    let frame = VideoFrame {
        width: 3,
        height: 2,
        pitch: 16,
        bgra: Arc::from([
            1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 0, 0, 0, 0, 10, 11, 12, 255, 13, 14, 15, 255,
            16, 17, 18, 255, 0, 0, 0, 0,
        ]),
    };
    let cropped = Rect {
        left: 1,
        top: 0,
        width: 2,
        height: 2,
    };
    {
        let state = m.ext.get_mut::<State>();
        let state = state.graphs.get_mut(&graph).unwrap();
        state.video.source = Some(cropped);
        state.video_frame = Some(frame);
        state.state = graph::PAUSED;
    }
    args(&mut m, &[graph, DATA, 0]);
    assert_eq!(current_image(&mut tests::env(&mut m), graph, &info()), S_OK);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 56);
    m.cpu.write(DATA + 128, &[0xcc; 56]).unwrap();
    m.cpu.write_u32(DATA, 55).unwrap();
    args(&mut m, &[graph, DATA, DATA + 128]);
    assert_eq!(
        current_image(&mut tests::env(&mut m), graph, &info()),
        E_OUTOFMEMORY
    );
    let mut unchanged = [0; 56];
    m.cpu.read(DATA + 128, &mut unchanged).unwrap();
    assert_eq!(unchanged, [0xcc; 56]);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 55);
    m.cpu.write_u32(DATA, 56).unwrap();
    for bad in [DATA + 4090, u32::MAX - 16] {
        args(&mut m, &[graph, DATA, bad]);
        assert_eq!(
            current_image(&mut tests::env(&mut m), graph, &info()),
            E_POINTER
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 56);
    }
    args(&mut m, &[graph, DATA, DATA + 128]);
    assert_eq!(current_image(&mut tests::env(&mut m), graph, &info()), S_OK);
    let mut image = [0; 56];
    m.cpu.read(DATA + 128, &mut image).unwrap();
    assert_eq!(&image[0..4], &40u32.to_le_bytes());
    assert_eq!(&image[4..8], &2i32.to_le_bytes());
    assert_eq!(&image[8..12], &2i32.to_le_bytes());
    assert_eq!(
        &image[40..],
        &[13, 14, 15, 255, 16, 17, 18, 255, 4, 5, 6, 255, 7, 8, 9, 255]
    );
    m.ext
        .get_mut::<State>()
        .graphs
        .get_mut(&graph)
        .unwrap()
        .state = graph::STOPPED;
    assert_eq!(
        current_image(&mut tests::env(&mut m), graph, &info()),
        VFW_E_NOT_PAUSED
    );
    assert!(m.ext.get::<State>().graphs[&graph].video_frame.is_some());
}
