//! The filter graph manager. Positions are 100 ns media time; the graph runs
//! without a reference clock and paces its own streams by the guest clock.
use std::collections::VecDeque;

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Handle, Object, Outcome, Wait, WakeReason};
use adv_os::com::Iid;

use super::*;

mod render;
pub(super) mod source;

pub(crate) const STOPPED: u32 = 0;
pub(crate) const PAUSED: u32 = 1;
pub(crate) const RUNNING: u32 = 2;

const EC_COMPLETE: u32 = 1;
const AM_MEDIAEVENT_NONOTIFY: u32 = 1;
const EC_USERABORT: u32 = 2;
const EC_ERRORABORT: u32 = 3;
const E_ABORT: u32 = 0x8000_4004;
const TIME_FORMAT_MEDIA_TIME: Iid = guid(
    0x7b78_5574,
    0x8c82,
    0x11cf,
    [0xbc, 0x0c, 0x00, 0xaa, 0x00, 0xac, 0x74, 0xf6],
);
const TIME_FORMAT_NONE: Iid = GUID_NULL;

pub(crate) struct Member {
    pub filter: u32,
    pub name: Vec<u16>,
    /// The host filter's canonical pointer; guest filters are called through vtables.
    pub host: Option<u32>,
}

pub(crate) struct Graph {
    /// IFilterGraph2, IMediaControl, IMediaEventEx, IMediaSeeking, IMediaPosition,
    /// IBasicAudio, IMediaEventSink, IMediaFilter, IBasicVideo, IVideoWindow.
    pub faces: Vec<u32>,
    pub members: Vec<Member>,
    pub state: u32,
    events: VecDeque<(u32, u32, u32)>,
    event: Handle,
    notify: Option<(u32, u32, u32)>,
    /// AM_MEDIAEVENT_NONOTIFY: events are neither queued, signaled nor posted.
    no_notify: bool,
    default_complete: bool,
    time_format: Iid,
    /// Media position at `origin`, or the frozen position when not running.
    position: i64,
    origin: Option<u64>,
    pub stop: Option<i64>,
    rate: f64,
    pub volume: i32,
    pub balance: i32,
    /// Renderers expected and seen to report completion for the current run.
    pub expected: u32,
    pub completed: u32,
    complete_sent: bool,
    pub video: video::window::WindowState,
    pub video_frame: Option<adv_device::surface::VideoFrame>,
    pub video_geometry: Option<VideoWindowGeometry>,
}

impl Graph {
    pub fn position(&self, now_us: u64) -> i64 {
        match self.origin {
            Some(origin) if self.state == RUNNING => {
                self.position + (now_us.saturating_sub(origin) as f64 * 10.0 * self.rate) as i64
            }
            _ => self.position,
        }
    }

    fn set_position(&mut self, position: i64, now_us: u64) {
        self.position = position.max(0);
        if self.origin.is_some() {
            self.origin = Some(now_us);
        }
    }
}

const GRAPH_FACES: [Face; 10] = [
    Face::FilterGraph,
    Face::MediaControl,
    Face::MediaEvent,
    Face::MediaSeeking,
    Face::MediaPosition,
    Face::BasicAudio,
    Face::MediaEventSink,
    Face::MediaFilter,
    Face::BasicVideo,
    Face::VideoWindow,
];

pub(crate) fn create(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    if outer != 0 {
        return Outcome::Return(CLASS_E_NOAGGREGATION);
    }
    let faces = match super::create(env, &GRAPH_FACES, Obj::Graph) {
        Ok(faces) => faces,
        Err(error) => return Outcome::Return(error),
    };
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    let canonical = faces[0];
    state(env).graphs.insert(
        canonical,
        Graph {
            faces,
            members: Vec::new(),
            state: STOPPED,
            events: VecDeque::new(),
            event,
            notify: None,
            no_notify: false,
            default_complete: true,
            time_format: TIME_FORMAT_MEDIA_TIME,
            position: 0,
            origin: None,
            stop: None,
            rate: 1.0,
            volume: 0,
            balance: 0,
            expected: 0,
            completed: 0,
            complete_sent: false,
            video: video::window::WindowState::default(),
            video_frame: None,
            video_geometry: None,
        },
    );
    let com = state(env).com;
    let outcome = match com.query::<Obj>(&mut env.sys.objects, canonical, iid) {
        Ok(pointer) => {
            let _ = env.cpu.write_u32(output, pointer);
            S_OK
        }
        Err(_) => E_NOINTERFACE,
    };
    // Drop the creation reference; the query holds the caller's.
    release(env, canonical);
    log::info!("DirectShow: filter graph {canonical:#x} created");
    Outcome::Return(outcome)
}

/// Release for every DirectShow face. The last graph reference first stops and
/// detaches its filters, which may call back into guest filters.
pub(crate) fn release_method(env: &mut Env<'_>, this: u32) -> Outcome {
    let graph = canonical(env, this).filter(|c| state(env).graphs.contains_key(c));
    let com = state(env).com;
    let last = graph.is_some_and(|_| {
        com.identity::<Obj>(&env.sys.objects, this)
            .and_then(|id| com.references(&env.sys.objects, id))
            == Some(1)
    });
    if let (Some(graph), true) = (graph, last) {
        return teardown(
            env,
            graph,
            Box::new(move |env, _| Outcome::Return(release(env, this))),
        );
    }
    Outcome::Return(release(env, this))
}

fn teardown(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    stop_sessions(env, graph);
    video::detach(env, graph);
    g(env, graph).state = STOPPED;
    let members: Vec<(u32, Option<u32>)> = state(env).graphs[&graph]
        .members
        .iter()
        .map(|m| (m.filter, m.host))
        .collect();
    let mut calls = VecDeque::new();
    for &(filter, host) in &members {
        if host.is_none() {
            calls.push_back(Call(filter, 4, vec![]));
        }
    }
    for &(_, host) in &members {
        if let Some(host) = host {
            calls.extend(filter::disconnect_filter(env, host));
        }
    }
    for &(filter, host) in &members {
        if host.is_none() {
            calls.push_back(Call(filter, 13, vec![0, 0]));
            calls.push_back(Call(filter, 2, vec![]));
        }
    }
    state(env).graphs.get_mut(&graph).unwrap().members.clear();
    let event = state(env).graphs[&graph].event;
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            for (_, host) in members {
                if let Some(host) = host {
                    if let Some(filter) = state(env).filters.get_mut(&host) {
                        filter.graph = 0;
                    }
                    release(env, host);
                }
            }
            env.sys.objects.close(event);
            then(env, results)
        }),
    )
}

fn graph_of(env: &Env<'_>, this: u32) -> Option<u32> {
    let graph = canonical(env, this)?;
    env.ext
        .get::<State>()
        .graphs
        .contains_key(&graph)
        .then_some(graph)
}

fn g<'a>(env: &'a mut Env<'_>, graph: u32) -> &'a mut Graph {
    state(env).graphs.get_mut(&graph).unwrap()
}

pub(crate) fn call(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    let graph = graph_of(env, this)?;
    let r = |value: u32| Some(Outcome::Return(value));
    match (face, method) {
        (Face::FilterGraph, "AddFilter") => Some(add_filter(env, graph, env.arg(1), env.arg(2))),
        (Face::FilterGraph, "RemoveFilter") => Some(remove_filter(env, graph, env.arg(1))),
        (Face::FilterGraph, "EnumFilters") => r(enum_filters(env, graph, env.arg(1))),
        (Face::FilterGraph, "FindFilterByName") => {
            Some(find_filter(env, graph, env.arg(1), env.arg(2)))
        }
        (Face::FilterGraph, "ConnectDirect") => {
            Some(filter::connect(env, env.arg(1), env.arg(2), env.arg(3)))
        }
        (Face::FilterGraph, "Connect") => Some(filter::connect(env, env.arg(1), env.arg(2), 0)),
        (Face::FilterGraph, "Disconnect") => Some(filter::disconnect(env, env.arg(1))),
        (Face::FilterGraph, "Reconnect" | "ReconnectEx") => r(E_NOTIMPL),
        (Face::FilterGraph, "SetDefaultSyncSource") => r(S_OK),
        (Face::FilterGraph, "AddSourceFilter") => {
            Some(source::add(env, graph, env.arg(1), env.arg(2), env.arg(3)))
        }
        (Face::FilterGraph, "Render") => Some(render::pin(env, graph, env.arg(1))),
        (Face::FilterGraph, "RenderFile") => Some(render::render_file(env, graph, env.arg(1))),
        (Face::FilterGraph, "RenderEx") => Some(render::extended(
            env,
            graph,
            env.arg(1),
            env.arg(2),
            env.arg(3),
        )),
        (Face::FilterGraph, "SetLogFile") => r(S_OK),
        (Face::FilterGraph, "Abort") => r(S_OK),
        (Face::FilterGraph, "ShouldOperationContinue") => r(S_OK),

        (Face::MediaControl, "Run") => {
            Some(run(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaControl, "Pause") => {
            Some(pause(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaControl, "Stop") => {
            Some(stop(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaControl, "StopWhenReady") => {
            Some(stop(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaControl, "GetState") => {
            let value = g(env, graph).state;
            let output = env.arg(2);
            if output == 0 {
                return r(E_POINTER);
            }
            let _ = env.cpu.write_u32(output, value);
            r(S_OK)
        }
        (Face::MediaControl, "RenderFile") => Some(render::render_file(env, graph, env.arg(1))),
        (Face::MediaControl, "AddSourceFilter") => {
            adv_abi::warn_unavailable(
                env,
                "IMediaControl::AddSourceFilter",
                "filter automation objects are unavailable",
            );
            r(E_NOTIMPL)
        }
        (Face::MediaFilter, "GetClassID") => {
            let output = env.arg(1);
            let _ = write_guid(env, output, &super::CLSID_FILTERGRAPH);
            r(S_OK)
        }
        (Face::MediaFilter, "Stop") => {
            Some(stop(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaFilter, "Pause") => {
            Some(pause(env, graph, Box::new(|_, _| Outcome::Return(S_OK))))
        }
        (Face::MediaFilter, "Run") => Some(run(env, graph, Box::new(|_, _| Outcome::Return(S_OK)))),
        (Face::MediaFilter, "GetState") => {
            let value = g(env, graph).state;
            let _ = env.cpu.write_u32(env.arg(2), value);
            r(S_OK)
        }
        (Face::MediaFilter, "SetSyncSource") => r(S_OK),
        (Face::MediaFilter, "GetSyncSource") => {
            let _ = env.cpu.write_u32(env.arg(1), 0);
            r(S_FALSE)
        }

        (Face::MediaEvent, method) => event_method(env, graph, method),
        (Face::MediaEventSink, "Notify") => {
            notify(env, graph, env.arg(1), env.arg(2), env.arg(3));
            Some(with_posts(env, S_OK))
        }
        (Face::MediaSeeking, method) => seeking(env, graph, method),
        (Face::MediaPosition, method) => position_method(env, graph, method),
        (Face::BasicAudio, method) => audio_method(env, graph, method),
        (Face::BasicVideo | Face::VideoWindow, method) => {
            video::window::call(env, graph, face, method)
        }
        _ => None,
    }
}

fn add_filter(env: &mut Env<'_>, graph: u32, filter: u32, name: u32) -> Outcome {
    if filter == 0 {
        return Outcome::Return(E_POINTER);
    }
    let name_text = read_wide(env, name, 127);
    if g(env, graph).members.iter().any(|m| m.filter == filter) {
        return Outcome::Return(E_INVALIDARG);
    }
    let host = canonical(env, filter).filter(|c| state(env).filters.contains_key(c));
    let graph_face = g(env, graph).faces[0];
    if let Some(host) = host {
        add_ref(env, host);
        let f = state(env).filters.get_mut(&host).unwrap();
        f.graph = graph;
        f.name = name_text.clone();
        g(env, graph).members.push(Member {
            filter,
            name: name_text,
            host: Some(host),
        });
        return Outcome::Return(S_OK);
    }
    let calls = VecDeque::from([
        Call(filter, 1, vec![]),
        Call(filter, 13, vec![graph_face, name]),
    ]);
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            let joined = results[1];
            if joined & 0x8000_0000 != 0 {
                return guest_call(env, filter, 2, vec![], move |_, _| Outcome::Return(joined));
            }
            if let Some(graph) = state(env).graphs.get_mut(&graph) {
                graph.members.push(Member {
                    filter,
                    name: name_text,
                    host: None,
                });
            }
            Outcome::Return(S_OK)
        }),
    )
}

fn remove_filter(env: &mut Env<'_>, graph: u32, filter: u32) -> Outcome {
    let Some(index) = g(env, graph)
        .members
        .iter()
        .position(|m| m.filter == filter)
    else {
        return Outcome::Return(VFW_E_NOT_FOUND);
    };
    let member = g(env, graph).members.remove(index);
    if let Some(host) = member.host {
        if state(env)
            .filters
            .get(&host)
            .is_some_and(|f| f.role == filter::Role::VideoRenderer)
        {
            video::detach(env, graph);
        }
        let calls = filter::disconnect_filter(env, host);
        if let Some(f) = state(env).filters.get_mut(&host) {
            f.graph = 0;
        }
        return chain(
            env,
            calls,
            Vec::new(),
            Box::new(move |env, _| {
                release(env, host);
                Outcome::Return(S_OK)
            }),
        );
    }
    let calls = VecDeque::from([Call(filter, 13, vec![0, 0]), Call(filter, 2, vec![])]);
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(|_, _| Outcome::Return(S_OK)),
    )
}

fn enum_filters(env: &mut Env<'_>, graph: u32, output: u32) -> u32 {
    if output == 0 {
        return E_POINTER;
    }
    let list = g(env, graph).members.iter().map(|m| m.filter).collect();
    match super::create(
        env,
        &[Face::EnumFilters],
        Obj::EnumFilters { list, position: 0 },
    ) {
        Ok(faces) => {
            let _ = env.cpu.write_u32(output, faces[0]);
            S_OK
        }
        Err(error) => error,
    }
}

fn find_filter(env: &mut Env<'_>, graph: u32, name: u32, output: u32) -> Outcome {
    if output == 0 {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    let name = read_wide(env, name, 127);
    let Some(filter) = g(env, graph)
        .members
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.filter)
    else {
        return Outcome::Return(VFW_E_NOT_FOUND);
    };
    let _ = env.cpu.write_u32(output, filter);
    guest_call(env, filter, 1, vec![], |_, _| Outcome::Return(S_OK))
}

/// Calls `slot` on every guest filter, renderers (last added) first.
fn each_guest_filter(
    env: &mut Env<'_>,
    graph: u32,
    slot: u32,
    args: Vec<u32>,
    then: Then,
) -> Outcome {
    let calls = g(env, graph)
        .members
        .iter()
        .rev()
        .filter(|m| m.host.is_none())
        .map(|m| Call(m.filter, slot, args.clone()))
        .collect();
    chain(env, calls, Vec::new(), then)
}

fn set_host_states(env: &mut Env<'_>, graph: u32, value: u32) {
    let hosts: Vec<u32> = g(env, graph)
        .members
        .iter()
        .filter_map(|m| m.host)
        .collect();
    for host in hosts {
        if let Some(filter) = state(env).filters.get_mut(&host) {
            filter.state = value;
        }
    }
}

pub(crate) fn pause(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let now = env.sys.clock.now_us();
    let previous = g(env, graph).state;
    if previous == PAUSED {
        return then(env, Vec::new());
    }
    if previous == STOPPED {
        let hr = video::auto_show(env, graph);
        if hr != S_OK {
            return Outcome::Return(hr);
        }
    }
    each_guest_filter(
        env,
        graph,
        5,
        vec![],
        Box::new(move |env, _| {
            let graph_state = g(env, graph);
            graph_state.position = graph_state.position(now);
            graph_state.origin = None;
            graph_state.state = PAUSED;
            set_host_states(env, graph, PAUSED);
            if previous == STOPPED {
                start_sessions(env, graph);
            } else {
                wake_sessions(env, graph);
            }
            then(env, Vec::new())
        }),
    )
}

pub(crate) fn run(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let previous = g(env, graph).state;
    if previous == RUNNING {
        return then(env, Vec::new());
    }
    if previous == STOPPED {
        return pause(env, graph, Box::new(move |env, _| run(env, graph, then)));
    }
    // Without a reference clock tStart only orders samples; stream time starts at 0.
    each_guest_filter(
        env,
        graph,
        6,
        vec![0, 0],
        Box::new(move |env, _| {
            let now = env.sys.clock.now_us();
            let graph_state = g(env, graph);
            graph_state.origin = Some(now);
            graph_state.state = RUNNING;
            set_host_states(env, graph, RUNNING);
            wake_sessions(env, graph);
            then(env, Vec::new())
        }),
    )
}

pub(crate) fn stop(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let now = env.sys.clock.now_us();
    if g(env, graph).state == STOPPED {
        return then(env, Vec::new());
    }
    {
        let graph_state = g(env, graph);
        graph_state.position = graph_state.position(now);
        graph_state.origin = None;
        graph_state.state = STOPPED;
    }
    set_host_states(env, graph, STOPPED);
    stop_sessions(env, graph);
    each_guest_filter(env, graph, 4, vec![], then)
}

fn start_sessions(env: &mut Env<'_>, graph: u32) {
    let hosts: Vec<u32> = g(env, graph)
        .members
        .iter()
        .filter_map(|m| m.host)
        .collect();
    let mut expected = 0;
    for host in hosts {
        expected += stream::start(env, graph, host);
    }
    let graph_state = g(env, graph);
    graph_state.expected = expected;
    graph_state.completed = 0;
    graph_state.complete_sent = false;
}

fn sessions_of(env: &mut Env<'_>, graph: u32) -> Vec<u32> {
    state(env)
        .sessions
        .iter()
        .filter(|(_, s)| s.graph == graph)
        .map(|(&id, _)| id)
        .collect()
}

pub(crate) fn wake_sessions(env: &mut Env<'_>, graph: u32) {
    for id in sessions_of(env, graph) {
        stream::wake(env, id);
    }
}

fn stop_sessions(env: &mut Env<'_>, graph: u32) {
    for id in sessions_of(env, graph) {
        stream::stop(env, id);
    }
}

/// IMediaEventSink::Notify and host renderer completion.
pub(crate) fn notify(env: &mut Env<'_>, graph: u32, code: u32, p1: u32, p2: u32) {
    let Some(graph_state) = state(env).graphs.get_mut(&graph) else {
        return;
    };
    match code {
        EC_COMPLETE if graph_state.default_complete => {
            graph_state.completed += 1;
            if graph_state.completed >= graph_state.expected && !graph_state.complete_sent {
                graph_state.complete_sent = true;
                queue_event(env, graph, EC_COMPLETE, S_OK, 0);
            }
        }
        EC_COMPLETE | EC_USERABORT | EC_ERRORABORT => queue_event(env, graph, code, p1, p2),
        code if code >= 0x8000 => queue_event(env, graph, code, p1, p2),
        // Renderer housekeeping (repaint, clock, quality, display) is default-handled.
        _ => log::debug!("DirectShow: event {code:#x} handled by the graph"),
    }
}

fn queue_event(env: &mut Env<'_>, graph: u32, code: u32, p1: u32, p2: u32) {
    log::info!("DirectShow: graph {graph:#x} event {code:#x}");
    let graph_state = g(env, graph);
    if graph_state.no_notify {
        return;
    }
    graph_state.events.push_back((code, p1, p2));
    let (event, notify) = (graph_state.event, graph_state.notify);
    env.sys.set_event(event);
    if let Some((window, message, instance)) = notify
        && window != 0
    {
        post_message(env, window, message, 0, instance);
    }
}

/// Post through the guest-visible user32 export; this front does not own windows.
fn post_message(env: &mut Env<'_>, window: u32, message: u32, wparam: u32, lparam: u32) {
    let Ok(target) = env
        .sys
        .thunks
        .resolve(env.cpu, "user32.dll", "PostMessageA")
    else {
        return;
    };
    // A notification is fire-and-forget; queue it on the next guest call boundary.
    state(env)
        .pending_posts
        .push((target, [window, message, wparam, lparam]));
}

/// Deliver queued window notifications before returning `value`.
fn with_posts(env: &mut Env<'_>, value: u32) -> Outcome {
    flush_posts(env, Box::new(move |_, _| Outcome::Return(value)))
}

pub(crate) fn flush_posts(env: &mut Env<'_>, then: Then) -> Outcome {
    if state(env).pending_posts.is_empty() {
        return then(env, Vec::new());
    }
    let (target, args) = state(env).pending_posts.remove(0);
    Outcome::call(target, args.to_vec(), move |env, _| flush_posts(env, then))
}

fn event_method(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "GetEventHandle" => {
            let event = g(env, graph).event;
            let output = env.arg(1);
            if output == 0 {
                return r(E_POINTER);
            }
            let _ = env.cpu.write_u32(output, event);
            r(S_OK)
        }
        "GetEvent" => {
            let (code, p1, p2, timeout) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
            Some(get_event(env, graph, [code, p1, p2], timeout))
        }
        "WaitForCompletion" => {
            let (timeout, output) = (env.arg(1), env.arg(2));
            Some(wait_completion(env, graph, timeout, output))
        }
        "CancelDefaultHandling" => {
            if env.arg(1) == EC_COMPLETE {
                g(env, graph).default_complete = false;
            }
            r(S_OK)
        }
        "RestoreDefaultHandling" => {
            if env.arg(1) == EC_COMPLETE {
                g(env, graph).default_complete = true;
            }
            r(S_OK)
        }
        "FreeEventParams" => r(S_OK),
        "SetNotifyWindow" => {
            let (window, message, instance) = (env.arg(1), env.arg(2), env.arg(3));
            g(env, graph).notify = Some((window, message, instance));
            r(S_OK)
        }
        "SetNotifyFlags" => match env.arg(1) {
            flags @ (0 | AM_MEDIAEVENT_NONOTIFY) => {
                g(env, graph).no_notify = flags != 0;
                r(S_OK)
            }
            _ => r(E_INVALIDARG),
        },
        "GetNotifyFlags" => {
            let flags = if g(env, graph).no_notify {
                AM_MEDIAEVENT_NONOTIFY
            } else {
                0
            };
            r(if env.cpu.write_u32(env.arg(1), flags).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        _ => None,
    }
}

fn deadline(env: &Env<'_>, timeout: u32) -> Option<u64> {
    (timeout != u32::MAX).then(|| env.sys.clock.now_us() + u64::from(timeout) * 1000)
}

fn get_event(env: &mut Env<'_>, graph: u32, outputs: [u32; 3], timeout: u32) -> Outcome {
    let Some(graph_state) = state(env).graphs.get_mut(&graph) else {
        return Outcome::Return(E_ABORT);
    };
    if let Some((code, p1, p2)) = graph_state.events.pop_front() {
        if graph_state.events.is_empty() {
            let event = graph_state.event;
            env.sys.reset_event(event);
        }
        for (output, value) in outputs.into_iter().zip([code, p1, p2]) {
            let _ = env.cpu.write_u32(output, value);
        }
        return Outcome::Return(S_OK);
    }
    if timeout == 0 {
        return Outcome::Return(E_ABORT);
    }
    let handle = graph_state.event;
    let until = deadline(env, timeout);
    Outcome::block(
        Wait::Object {
            handle,
            deadline: until,
        },
        move |env, resume| {
            if matches!(resume, adv_machine::Resume::Woke(WakeReason::Timeout)) {
                return get_event(env, graph, outputs, 0);
            }
            let left = until.map_or(u32::MAX, |until| {
                (until.saturating_sub(env.sys.clock.now_us()) / 1000) as u32
            });
            get_event(env, graph, outputs, left.max(1))
        },
    )
}

fn wait_completion(env: &mut Env<'_>, graph: u32, timeout: u32, output: u32) -> Outcome {
    let Some(graph_state) = state(env).graphs.get(&graph) else {
        return Outcome::Return(E_ABORT);
    };
    if let Some(index) = graph_state
        .events
        .iter()
        .position(|&(code, ..)| matches!(code, EC_COMPLETE | EC_USERABORT | EC_ERRORABORT))
    {
        let (code, ..) = g(env, graph).events.remove(index).unwrap();
        let _ = env.cpu.write_u32(output, code);
        return Outcome::Return(S_OK);
    }
    if timeout == 0 || graph_state.state != RUNNING {
        return Outcome::Return(E_ABORT);
    }
    let handle = graph_state.event;
    let until = deadline(env, timeout);
    Outcome::block(
        Wait::Object {
            handle,
            deadline: until,
        },
        move |env, _| {
            let left = until.map_or(u32::MAX, |until| {
                (until.saturating_sub(env.sys.clock.now_us()) / 1000) as u32
            });
            wait_completion(env, graph, left, output)
        },
    )
}

pub(crate) fn duration(env: &Env<'_>, graph: u32) -> i64 {
    let state = env.ext.get::<State>();
    state.graphs[&graph]
        .members
        .iter()
        .filter_map(|m| state.filters.get(&m.host?))
        .filter_map(|f| f.source.as_ref())
        .map(|s| s.duration_us as i64 * 10)
        .max()
        .unwrap_or(0)
}

pub(crate) fn current(env: &Env<'_>, graph: u32) -> i64 {
    let now = env.sys.clock.now_us();
    let length = duration(env, graph);
    let graph_state = &env.ext.get::<State>().graphs[&graph];
    let end = graph_state
        .stop
        .unwrap_or(if length > 0 { length } else { i64::MAX });
    graph_state.position(now).min(end)
}

/// Container duration is only an estimate (short MPEG-PS probes can report the
/// first PES span). Clamping decoding to that estimate would strand a future
/// frame forever and prevent EC_COMPLETE. Only an explicit segment stop bounds
/// delivery; otherwise the decoder's actual EOF completes the stream.
pub(crate) fn stream_position(env: &Env<'_>, graph: u32) -> i64 {
    let graph = &env.ext.get::<State>().graphs[&graph];
    graph
        .position(env.sys.clock.now_us())
        .min(graph.stop.unwrap_or(i64::MAX))
}

/// Seek every stream; flushing and new segments happen on the stream threads.
fn seek(env: &mut Env<'_>, graph: u32, position: Option<i64>, stop_position: Option<i64>) {
    let now = env.sys.clock.now_us();
    let graph_state = g(env, graph);
    if let Some(stop_position) = stop_position {
        graph_state.stop = Some(stop_position);
    }
    if let Some(position) = position {
        graph_state.set_position(position, now);
        graph_state.completed = 0;
        graph_state.complete_sent = false;
        for id in sessions_of(env, graph) {
            stream::seek(env, id, position);
        }
    }
}

fn resolve_position(current: i64, value: i64, flags: u32) -> Option<i64> {
    match flags & 3 {
        0 => None,
        1 => Some(value),
        2 | 3 => Some(current + value),
        _ => None,
    }
}

fn seeking(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    match method {
        "GetCapabilities" | "CheckCapabilities" => {
            if method == "CheckCapabilities" {
                let wanted = env.cpu.read_u32(a1).unwrap_or(0);
                let _ = env.cpu.write_u32(a1, wanted & 0x3f);
                return r(if wanted & !0x3f == 0 { S_OK } else { S_FALSE });
            }
            let _ = env.cpu.write_u32(a1, 0x3f);
            r(S_OK)
        }
        "IsFormatSupported" => {
            let format = read_guid(env, a1).unwrap_or(GUID_NULL);
            r(if format == TIME_FORMAT_MEDIA_TIME {
                S_OK
            } else {
                S_FALSE
            })
        }
        "QueryPreferredFormat" => {
            let _ = write_guid(env, a1, &TIME_FORMAT_MEDIA_TIME);
            r(S_OK)
        }
        "GetTimeFormat" => {
            let format = g(env, graph).time_format;
            let _ = write_guid(env, a1, &format);
            r(S_OK)
        }
        "IsUsingTimeFormat" => {
            let format = read_guid(env, a1).unwrap_or(GUID_NULL);
            r(if format == g(env, graph).time_format {
                S_OK
            } else {
                S_FALSE
            })
        }
        "SetTimeFormat" => {
            let format = read_guid(env, a1).unwrap_or(GUID_NULL);
            if format != TIME_FORMAT_MEDIA_TIME && format != TIME_FORMAT_NONE {
                return r(E_INVALIDARG);
            }
            g(env, graph).time_format = TIME_FORMAT_MEDIA_TIME;
            r(S_OK)
        }
        "GetDuration" => {
            let value = duration(env, graph);
            r(if write_i64(env, a1, value) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetStopPosition" => {
            let value = g(env, graph).stop.unwrap_or(duration(env, graph));
            r(if write_i64(env, a1, value) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetCurrentPosition" => {
            let value = current(env, graph);
            r(if write_i64(env, a1, value) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "ConvertTimeFormat" => {
            let value = arg_i64(env, 3);
            r(if write_i64(env, a1, value) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "SetPositions" => {
            let (current_flags, stop_pointer, stop_flags) = (env.arg(2), env.arg(3), env.arg(4));
            let now_position = current(env, graph);
            let stop_now = g(env, graph).stop.unwrap_or(duration(env, graph));
            let position = read_i64(env, a1)
                .and_then(|value| resolve_position(now_position, value, current_flags));
            let stop_position = read_i64(env, stop_pointer)
                .and_then(|value| resolve_position(stop_now, value, stop_flags));
            seek(env, graph, position, stop_position);
            if current_flags & 8 != 0 {
                let value = current(env, graph);
                write_i64(env, a1, value);
            }
            if stop_flags & 8 != 0 {
                let value = g(env, graph).stop.unwrap_or(duration(env, graph));
                write_i64(env, stop_pointer, value);
            }
            r(S_OK)
        }
        "GetPositions" => {
            let value = current(env, graph);
            write_i64(env, a1, value);
            let value = g(env, graph).stop.unwrap_or(duration(env, graph));
            write_i64(env, env.arg(2), value);
            r(S_OK)
        }
        "GetAvailable" => {
            write_i64(env, a1, 0);
            let value = duration(env, graph);
            write_i64(env, env.arg(2), value);
            r(S_OK)
        }
        "SetRate" => {
            let rate = arg_f64(env, 1);
            if !(rate > 0.0) {
                return r(E_INVALIDARG);
            }
            let now = env.sys.clock.now_us();
            let graph_state = g(env, graph);
            graph_state.position = graph_state.position(now);
            if graph_state.origin.is_some() {
                graph_state.origin = Some(now);
            }
            graph_state.rate = rate;
            r(S_OK)
        }
        "GetRate" => {
            let rate = g(env, graph).rate;
            let _ = env.cpu.write(a1, &rate.to_le_bytes());
            r(S_OK)
        }
        "GetPreroll" => {
            write_i64(env, a1, 0);
            r(S_OK)
        }
        _ => None,
    }
}

/// IMediaPosition: REFTIME seconds as doubles, by value or through pointers.
fn position_method(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let seconds = |value: i64| value as f64 / 10_000_000.0;
    let write = |env: &mut Env<'_>, value: f64| {
        let _ = env.cpu.write(env.arg(1), &value.to_le_bytes());
    };
    match method {
        "get_Duration" => {
            let value = seconds(duration(env, graph));
            write(env, value);
            r(S_OK)
        }
        "get_CurrentPosition" => {
            let value = seconds(current(env, graph));
            write(env, value);
            r(S_OK)
        }
        "put_CurrentPosition" => {
            let value = (arg_f64(env, 1) * 10_000_000.0) as i64;
            seek(env, graph, Some(value), None);
            r(S_OK)
        }
        "get_StopTime" => {
            let value = seconds(g(env, graph).stop.unwrap_or(duration(env, graph)));
            write(env, value);
            r(S_OK)
        }
        "put_StopTime" => {
            let value = (arg_f64(env, 1) * 10_000_000.0) as i64;
            seek(env, graph, None, Some(value));
            r(S_OK)
        }
        "get_PrerollTime" => {
            write(env, 0.0);
            r(S_OK)
        }
        "put_PrerollTime" => r(S_OK),
        "put_Rate" => {
            let rate = arg_f64(env, 1);
            if rate > 0.0 {
                g(env, graph).rate = rate;
            }
            r(S_OK)
        }
        "get_Rate" => {
            let value = g(env, graph).rate;
            write(env, value);
            r(S_OK)
        }
        "CanSeekForward" | "CanSeekBackward" => {
            let _ = env.cpu.write_u32(env.arg(1), u32::MAX);
            r(S_OK)
        }
        _ => None,
    }
}

fn audio_method(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "put_Volume" => {
            let volume = env.arg(1) as i32;
            if !(-10000..=0).contains(&volume) {
                return r(E_INVALIDARG);
            }
            g(env, graph).volume = volume;
            stream::apply_volume(env, graph);
            r(S_OK)
        }
        "get_Volume" => {
            let value = g(env, graph).volume;
            let _ = env.cpu.write_u32(env.arg(1), value as u32);
            r(S_OK)
        }
        "put_Balance" => {
            let balance = env.arg(1) as i32;
            if !(-10000..=10000).contains(&balance) {
                return r(E_INVALIDARG);
            }
            g(env, graph).balance = balance;
            stream::apply_volume(env, graph);
            r(S_OK)
        }
        "get_Balance" => {
            let value = g(env, graph).balance;
            let _ = env.cpu.write_u32(env.arg(1), value as u32);
            r(S_OK)
        }
        _ => None,
    }
}
