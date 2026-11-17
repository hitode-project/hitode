//! The filter graph manager. Positions are 100 ns media time; the graph runs
//! without a reference clock and paces its own streams by the guest clock.
use std::collections::VecDeque;

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Handle, Object, Outcome, Wait};
use adv_os::com::Iid;

use super::*;

mod autoplug;
mod clock;
mod delegate;
pub(super) mod mapper;
mod render;
mod seeking;
pub(super) mod source;
mod state;

pub(crate) const STOPPED: u32 = 0;
pub(crate) const PAUSED: u32 = 1;
pub(crate) const RUNNING: u32 = 2;

const EC_COMPLETE: u32 = 1;
const AM_MEDIAEVENT_NONOTIFY: u32 = 1;
pub(crate) const EC_USERABORT: u32 = 2;
const EC_ERRORABORT: u32 = 3;
const EC_REPAINT: u32 = 5;
pub(super) const EC_CLOCK_CHANGED: u32 = 13;
const E_ABORT: u32 = 0x8000_4004;
const E_FAIL: u32 = 0x8000_4005;
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
    /// A guest member's IMediaSeeking once it answered in media time; 0 until then.
    pub seeking: u32,
}

pub(crate) struct Graph {
    /// [`GRAPH_FACES`], then the inner IUnknown of an aggregated graph.
    pub faces: Vec<u32>,
    /// The controlling outer object of an aggregated graph, and the graph's
    /// own non-delegating IUnknown; both zero otherwise.
    pub outer: u32,
    pub inner: u32,
    /// The aggregated FilterMapper2's inner IUnknown, once created.
    mapper: u32,
    pub members: Vec<Member>,
    /// IGraphVersion's count, advanced on each membership change.
    version: u32,
    /// The counter that generated filter names take, from 1 modulo 10000.
    name_index: u32,
    /// The guest site set through IObjectWithSite, holding one reference.
    site: u32,
    /// The reference clock set through IMediaFilter, holding one reference;
    /// every guest member receives it.
    clock: u32,
    /// Whether the graph still chooses its clock, and the member whose own
    /// clock it chose.
    default_clock: bool,
    clock_provider: u32,
    pub state: u32,
    stopping: bool,
    events: VecDeque<(u32, u32, u32)>,
    event: Handle,
    /// Signaled while no stream thread of the graph is running.
    pub(super) idle: Handle,
    /// Signaled once the run completes or aborts, for WaitForCompletion.
    finished: Handle,
    /// The code WaitForCompletion reports for the current run.
    completion: Option<u32>,
    public_event: Option<u32>,
    notify: Option<(u32, u32, u32)>,
    /// AM_MEDIAEVENT_NONOTIFY: events are neither queued, signaled nor posted.
    no_notify: bool,
    default_complete: bool,
    time_format: Iid,
    /// Media position at `origin`, or the frozen position when not running.
    position: i64,
    origin: Option<u64>,
    /// The run completed: the position rests at the stop until a seek.
    ended: bool,
    pub stop: Option<i64>,
    /// The stop position members reported when the run started.
    stream_stop: Option<i64>,
    rate: f64,
    pub volume: i32,
    pub balance: i32,
    /// Renderers expected and seen to report completion for the current run.
    pub expected: u32,
    host_expected: u32,
    pub completed: u32,
    complete_sent: bool,
    pending_run: Option<u64>,
    /// Duration reported by a guest source filter feeding a host decoder.
    pub guest_duration: i64,
    pub video: video::window::WindowState,
    pub video_frame: Option<adv_device::surface::VideoFrame>,
    pub video_geometry: Option<VideoWindowGeometry>,
}

impl Graph {
    /// The IUnknown that aggregated objects, such as the mapper, delegate to.
    fn controlling(&self) -> u32 {
        if self.outer != 0 {
            self.outer
        } else {
            self.faces[0]
        }
    }

    fn named(&self, name: &[u16]) -> bool {
        self.members.iter().any(|m| m.name == name)
    }

    /// A filter's name in the graph: `requested` when free, otherwise the
    /// first free "NNNN" or "requested NNNN" from the graph's counter.
    fn unique_name(&mut self, requested: Option<Vec<u16>>) -> Option<Vec<u16>> {
        if let Some(name) = requested.as_ref().filter(|n| !self.named(n)) {
            return Some(name.clone());
        }
        for _ in 0..10_000 {
            let suffix = format!("{:04}", self.name_index);
            self.name_index = (self.name_index + 1) % 10_000;
            let mut name = requested.clone().map_or_else(Vec::new, |mut n| {
                n.push(u16::from(b' '));
                n
            });
            name.extend(suffix.encode_utf16());
            if !self.named(&name) {
                return Some(name);
            }
        }
        None
    }

    pub(crate) fn join(&mut self, member: Member) {
        self.members.push(member);
        self.version = self.version.wrapping_add(1);
    }

    pub(crate) fn leave(&mut self, index: usize) -> Member {
        self.version = self.version.wrapping_add(1);
        self.members.remove(index)
    }
}

impl Graph {
    pub fn position(&self, now_us: u64) -> i64 {
        match self.origin {
            Some(origin) if self.state == RUNNING && !self.ended => {
                self.position + (now_us.saturating_sub(origin) as f64 * 10.0 * self.rate) as i64
            }
            _ => self.position,
        }
    }

    /// Members hold still: the graph is paused, or reports Running while its
    /// members finish pausing.
    pub(crate) fn members_paused(&self) -> bool {
        self.state == PAUSED || self.pending_run.is_some()
    }

    fn set_position(&mut self, position: i64, now_us: u64) {
        self.position = position.max(0);
        self.ended = false;
        if self.origin.is_some() {
            self.origin = Some(now_us);
        }
    }
}

const GRAPH_FACES: [Face; 14] = [
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
    Face::ObjectWithSite,
    Face::GraphConfig,
    Face::GraphVersion,
    Face::VideoFrameStep,
];

pub(crate) fn create(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    // Like Windows, an aggregated graph hands out only its inner IUnknown.
    if outer != 0 && iid != adv_os::com::IUNKNOWN {
        let _ = env.cpu.write_u32(output, 0);
        return Outcome::Return(E_NOINTERFACE);
    }
    let created = if outer == 0 {
        super::create(env, &GRAPH_FACES, Obj::Graph)
    } else {
        let mut faces = GRAPH_FACES.to_vec();
        faces.push(Face::Inner);
        super::create_aggregated(env, &faces, Obj::Graph)
    };
    let faces = match created {
        Ok(faces) => faces,
        Err(error) => return Outcome::Return(error),
    };
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    let idle = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: true,
    });
    let finished = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    let public_event = if let Some(host) = env.ext.try_get::<Host>() {
        match (host.event)(env) {
            Ok(event) => Some(event),
            Err(error) => return Outcome::Fail(error),
        }
    } else {
        None
    };
    let canonical = faces[0];
    let inner = if outer == 0 {
        0
    } else {
        *faces.last().unwrap()
    };
    state(env).graphs.insert(
        canonical,
        Graph {
            faces,
            outer,
            inner,
            mapper: 0,
            version: 0,
            name_index: 1,
            clock: 0,
            default_clock: true,
            clock_provider: 0,
            members: Vec::new(),
            site: 0,
            state: STOPPED,
            stopping: false,
            events: VecDeque::new(),
            event,
            idle,
            finished,
            completion: None,
            public_event,
            notify: None,
            no_notify: false,
            default_complete: true,
            time_format: TIME_FORMAT_MEDIA_TIME,
            position: 0,
            origin: None,
            ended: false,
            stop: None,
            stream_stop: None,
            rate: 1.0,
            volume: 0,
            balance: 0,
            expected: 0,
            host_expected: 0,
            completed: 0,
            complete_sent: false,
            pending_run: None,
            guest_duration: 0,
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

/// The inner IUnknown's QueryInterface: IUnknown is itself, and any other
/// interface is retained through the outer object, as COM aggregation needs.
pub(crate) fn inner_query(env: &mut Env<'_>, this: u32, iid: u32, output: u32) -> Outcome {
    if output == 0 || iid == 0 {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    let Ok(requested) = read_guid(env, iid) else {
        return Outcome::Return(E_POINTER);
    };
    if requested == adv_os::com::IUNKNOWN {
        return Outcome::Return(super::query_interface(env, this, iid, output));
    }
    if let Some(graph) = mapper::target(env, this, iid) {
        return mapper::query(env, graph, iid, output);
    }
    let com = state(env).com;
    let Ok(pointer) = com.resolve_interface::<Obj>(&mut env.sys.objects, this, requested) else {
        return Outcome::Return(E_NOINTERFACE);
    };
    let (outer, _) = super::aggregate(env, this).unwrap();
    guest_call(env, outer, 1, vec![], move |env, _| {
        let _ = env.cpu.write_u32(output, pointer);
        Outcome::Return(S_OK)
    })
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
    super::dmo::begin_stop(env, graph);
    super::vmr::begin_stop(env, graph);
    stop_sessions(env, graph);
    video::detach(env, graph);
    g(env, graph).state = STOPPED;
    g(env, graph).stopping = true;
    let members: Vec<(u32, Option<u32>)> = state(env).graphs[&graph]
        .members
        .iter()
        .map(|m| (m.filter, m.host))
        .collect();
    let cached: Vec<u32> = state(env).graphs[&graph]
        .members
        .iter()
        .map(|m| m.seeking)
        .filter(|&s| s != 0)
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
    let guests: Vec<u32> = members
        .iter()
        .filter(|m| m.1.is_none())
        .map(|m| m.0)
        .collect();
    let mut leave: VecDeque<Call> = cached.into_iter().map(|s| Call(s, 2, vec![])).collect();
    for &filter in &guests {
        leave.push_back(Call(filter, 13, vec![0, 0]));
        leave.push_back(Call(filter, 8, vec![0]));
        leave.push_back(Call(filter, 2, vec![]));
    }
    let clock = std::mem::take(&mut g(env, graph).clock);
    if clock != 0 {
        leave.push_back(Call(clock, 2, vec![]));
    }
    let site = std::mem::take(&mut g(env, graph).site);
    if site != 0 {
        leave.push_back(Call(site, 2, vec![]));
    }
    let mapper = std::mem::take(&mut g(env, graph).mapper);
    if mapper != 0 {
        leave.push_back(Call(mapper, 2, vec![]));
    }
    state(env).graphs.get_mut(&graph).unwrap().members.clear();
    let event = state(env).graphs[&graph].event;
    let public_event = state(env).graphs[&graph].public_event;
    let idle = state(env).graphs[&graph].idle;
    let finished = state(env).graphs[&graph].finished;
    let finish: Then = Box::new(move |env, results| {
        for (_, host) in members {
            if let Some(host) = host {
                if let Some(filter) = state(env).filters.get_mut(&host) {
                    filter.graph = 0;
                }
                release(env, host);
            }
        }
        env.sys.objects.close(event);
        env.sys.objects.close(idle);
        env.sys.objects.close(finished);
        if let Some(event) = public_event {
            host::close(env, event);
        }
        then(env, results)
    });
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, _| {
            disconnect_guests(
                env,
                guests.into(),
                Box::new(move |env| chain(env, leave, Vec::new(), finish)),
            )
        }),
    )
}

/// Like quartz's RemoveFilter, disconnect every connected pin of each guest
/// filter, peer first, so pin references no longer hold either filter.
fn disconnect_guests(
    env: &mut Env<'_>,
    mut filters: VecDeque<u32>,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    let Some(filter) = filters.pop_front() else {
        return then(env);
    };
    let Some(scratch) = alloc(env, 12) else {
        return then(env);
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, filter, 10, vec![scratch], move |env, hr| {
        let pins = env.cpu.read_u32(scratch).unwrap_or(0);
        let rest: Box<dyn FnOnce(&mut Env<'_>) -> Outcome> = Box::new(move |env| {
            free(env, scratch);
            disconnect_guests(env, filters, then)
        });
        if hr != S_OK || pins == 0 {
            return rest(env);
        }
        disconnect_pins(env, pins, scratch, rest)
    })
}

fn disconnect_pins(
    env: &mut Env<'_>,
    pins: u32,
    scratch: u32,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + 4, 0);
    guest_call(env, pins, 3, vec![1, scratch + 4, 0], move |env, hr| {
        let pin = env.cpu.read_u32(scratch + 4).unwrap_or(0);
        if hr != S_OK || pin == 0 {
            return guest_call(env, pins, 2, vec![], move |env, _| then(env));
        }
        let _ = env.cpu.write_u32(scratch + 8, 0);
        guest_call(env, pin, 6, vec![scratch + 8], move |env, _| {
            let peer = env.cpu.read_u32(scratch + 8).unwrap_or(0);
            let mut calls = VecDeque::new();
            if peer != 0 {
                calls.extend([
                    Call(peer, 5, vec![]),
                    Call(peer, 2, vec![]),
                    Call(pin, 5, vec![]),
                ]);
            }
            calls.push_back(Call(pin, 2, vec![]));
            chain(
                env,
                calls,
                Vec::new(),
                Box::new(move |env, _| disconnect_pins(env, pins, scratch, then)),
            )
        })
    })
}

/// Quartz keeps one reference to the site and answers GetSite by querying it;
/// players hand the graph key providers this way before loading protected media.
fn set_site(env: &mut Env<'_>, graph: u32, site: u32) -> Outcome {
    let old = std::mem::replace(&mut g(env, graph).site, site);
    let mut calls = VecDeque::new();
    if site != 0 {
        calls.push_back(Call(site, 1, vec![]));
    }
    if old != 0 {
        calls.push_back(Call(old, 2, vec![]));
    }
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(|_, _| Outcome::Return(S_OK)),
    )
}

fn get_site(env: &mut Env<'_>, graph: u32, iid: u32, output: u32) -> Outcome {
    if output == 0 {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    let site = g(env, graph).site;
    if site == 0 {
        return Outcome::Return(E_FAIL);
    }
    guest_call(env, site, 0, vec![iid, output], |_, hr| Outcome::Return(hr))
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
        (Face::FilterGraph, "Connect") => {
            Some(render::connect_pins(env, graph, env.arg(1), env.arg(2)))
        }
        (Face::FilterGraph, "Disconnect") => Some(filter::disconnect(env, env.arg(1))),
        (Face::FilterGraph, "Reconnect") => Some(reconnect(env, graph, env.arg(1), 0)),
        (Face::FilterGraph, "ReconnectEx") => Some(reconnect(env, graph, env.arg(1), env.arg(2))),
        (Face::FilterGraph, "SetDefaultSyncSource") => Some(clock::set_default(
            env,
            graph,
            Box::new(|_, hr| Outcome::Return(hr[0])),
        )),
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
        (Face::GraphVersion, "QueryVersion") => {
            let output = env.arg(1);
            if output == 0 {
                return r(E_POINTER);
            }
            let version = g(env, graph).version;
            let _ = env.cpu.write_u32(output, version);
            r(S_OK)
        }
        (Face::FilterGraph, "SetLogFile") => r(S_OK),
        (Face::FilterGraph, "Abort") => r(S_OK),
        (Face::FilterGraph, "ShouldOperationContinue") => r(S_OK),

        (Face::MediaControl, "Run") => Some(run(env, graph, state::reply())),
        (Face::MediaControl, "Pause") => Some(pause(env, graph, state::reply())),
        (Face::MediaControl, "Stop") => Some(stop(env, graph, state::reply())),
        (Face::MediaControl, "StopWhenReady") => Some(pause(
            env,
            graph,
            Box::new(move |env, paused| {
                let paused = state::combined(&paused);
                if paused & 0x8000_0000 != 0 {
                    return Outcome::Return(paused);
                }
                stop(
                    env,
                    graph,
                    Box::new(move |_, stopped| {
                        let stopped = state::combined(&stopped);
                        Outcome::Return(if stopped == S_OK { paused } else { stopped })
                    }),
                )
            }),
        )),
        (Face::MediaControl, "GetState") => {
            // A negative OAFilterState timeout waits forever.
            let timeout = env.arg(1);
            let timeout = if (timeout as i32) < 0 {
                u32::MAX
            } else {
                timeout
            };
            Some(state::get(env, graph, timeout, env.arg(2)))
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
        (Face::MediaFilter, "Stop") => Some(stop(env, graph, state::reply())),
        (Face::MediaFilter, "Pause") => Some(pause(env, graph, state::reply())),
        (Face::MediaFilter, "Run") => {
            let start = u64::from(env.arg(1)) | (u64::from(env.arg(2)) << 32);
            Some(run_at(env, graph, start, state::reply()))
        }
        (Face::MediaFilter, "GetState") => Some(state::get(env, graph, env.arg(1), env.arg(2))),
        (Face::MediaFilter, "SetSyncSource") => Some(clock::set(env, graph, env.arg(1))),
        (Face::ObjectWithSite, "SetSite") => Some(set_site(env, graph, env.arg(1))),
        (Face::ObjectWithSite, "GetSite") => Some(get_site(env, graph, env.arg(1), env.arg(2))),
        (Face::MediaFilter, "GetSyncSource") => {
            let output = env.arg(1);
            if output == 0 {
                return r(E_POINTER);
            }
            let clock = g(env, graph).clock;
            let _ = env.cpu.write_u32(output, clock);
            if clock == 0 {
                return r(S_FALSE);
            }
            Some(guest_call(env, clock, 1, vec![], |_, _| {
                Outcome::Return(S_OK)
            }))
        }

        (Face::MediaEvent, method) => event_method(env, graph, method),
        (Face::MediaEventSink, "Notify") => {
            notify(env, graph, env.arg(1), env.arg(2), env.arg(3));
            Some(with_posts(env, S_OK))
        }
        (Face::MediaSeeking, method) => seeking::call(env, graph, method),
        (Face::MediaPosition, method) => seeking::position(env, graph, method),
        (Face::BasicAudio, method) if host_audio(env, graph) => audio_method(env, graph, method),
        (Face::BasicVideo | Face::VideoWindow, method) if video::available(env, graph) => {
            video::window::call(env, graph, face, method)
        }
        (Face::BasicAudio | Face::BasicVideo | Face::VideoWindow, method) => {
            Some(delegate::call(env, graph, face, method))
        }
        _ => None,
    }
}

fn add_filter(env: &mut Env<'_>, graph: u32, filter: u32, name: u32) -> Outcome {
    add_filter_then(env, graph, filter, name, |_, hr| Outcome::Return(hr))
}

/// IFilterGraph::AddFilter, then `then` with its HRESULT.
fn add_filter_then(
    env: &mut Env<'_>,
    graph: u32,
    filter: u32,
    name: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    if filter == 0 {
        return then(env, E_POINTER);
    }
    let requested = (name != 0).then(|| read_wide(env, name, 127));
    if g(env, graph).members.iter().any(|m| m.filter == filter) {
        return then(env, E_INVALIDARG);
    }
    let duplicate = requested.as_ref().is_some_and(|n| g(env, graph).named(n));
    let Some(name_text) = g(env, graph).unique_name(requested) else {
        return then(env, VFW_E_DUPLICATE_NAME);
    };
    let added = if duplicate {
        VFW_S_DUPLICATE_NAME
    } else {
        S_OK
    };
    let host = canonical(env, filter).filter(|c| state(env).filters.contains_key(c));
    let graph_face = g(env, graph).faces[0];
    if let Some(host) = host {
        add_ref(env, host);
        let name = name_text.clone();
        return filter::join_graph(
            env,
            host,
            graph,
            name_text,
            Box::new(move |env, hr| {
                if hr & 0x8000_0000 != 0 {
                    release(env, host);
                    return then(env, hr);
                }
                if state(env).graphs.contains_key(&graph) {
                    g(env, graph).join(Member {
                        filter,
                        name,
                        host: Some(host),
                        seeking: 0,
                    });
                }
                then(env, added)
            }),
        );
    }
    // The filter receives the final name; filters copy it in JoinFilterGraph.
    let bytes: Vec<u8> = name_text
        .iter()
        .chain(&[0])
        .flat_map(|c| c.to_le_bytes())
        .collect();
    let Some(name) = alloc(env, bytes.len() as u32) else {
        return then(env, E_OUTOFMEMORY);
    };
    let _ = env.cpu.write(name, &bytes);
    let calls = VecDeque::from([
        Call(filter, 1, vec![]),
        Call(filter, 13, vec![graph_face, name]),
    ]);
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            free(env, name);
            let joined = results[1];
            if joined & 0x8000_0000 != 0 {
                return guest_call(env, filter, 2, vec![], move |env, _| then(env, joined));
            }
            let Some(graph) = state(env).graphs.get_mut(&graph) else {
                return then(env, added);
            };
            graph.join(Member {
                filter,
                name: name_text,
                host: None,
                seeking: 0,
            });
            let clock = graph.clock;
            guest_call(env, filter, 8, vec![clock], move |env, _| then(env, added))
        }),
    )
}

pub(super) fn remove_filter(env: &mut Env<'_>, graph: u32, filter: u32) -> Outcome {
    if !g(env, graph).members.iter().any(|m| m.filter == filter) {
        return Outcome::Return(VFW_E_NOT_FOUND);
    }
    clock::leave(
        env,
        graph,
        filter,
        Box::new(move |env, _| detach(env, graph, filter)),
    )
}

fn detach(env: &mut Env<'_>, graph: u32, filter: u32) -> Outcome {
    let Some(index) = g(env, graph)
        .members
        .iter()
        .position(|m| m.filter == filter)
    else {
        return Outcome::Return(VFW_E_NOT_FOUND);
    };
    let member = g(env, graph).leave(index);
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
    let mut calls = VecDeque::from([
        Call(filter, 13, vec![0, 0]),
        Call(filter, 8, vec![0]),
        Call(filter, 2, vec![]),
    ]);
    if member.seeking != 0 {
        calls.push_front(Call(member.seeking, 2, vec![]));
    }
    disconnect_guests(
        env,
        VecDeque::from([filter]),
        Box::new(move |env| {
            chain(
                env,
                calls,
                Vec::new(),
                Box::new(|_, _| Outcome::Return(S_OK)),
            )
        }),
    )
}

/// IFilterGraph2::ReconnectEx: disconnect both ends of `pin`'s connection and
/// connect them directly again with `mt`; refused unless the graph is stopped.
fn reconnect(env: &mut Env<'_>, graph: u32, pin: u32, mt: u32) -> Outcome {
    if g(env, graph).state != STOPPED {
        return Outcome::Return(VFW_E_WRONG_STATE);
    }
    let Some(scratch) = alloc(env, 4) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let _ = env.cpu.write_u32(scratch, 0);
    // IPin::ConnectedTo, then IPin::QueryDirection.
    guest_call(env, pin, 6, vec![scratch], move |env, hr| {
        let peer = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || peer == 0 {
            free(env, scratch);
            return Outcome::Return(if hr == S_OK { VFW_E_NOT_CONNECTED } else { hr });
        }
        guest_call(env, pin, 9, vec![scratch], move |env, _| {
            let output = env.cpu.read_u32(scratch).unwrap_or(0) == filter::OUTPUT;
            free(env, scratch);
            let first = filter::disconnect(env, peer);
            after(
                env,
                first,
                Box::new(move |env, _| {
                    let second = filter::disconnect(env, pin);
                    after(
                        env,
                        second,
                        Box::new(move |env, _| {
                            let (from, to) = if output { (pin, peer) } else { (peer, pin) };
                            let connected = filter::connect(env, from, to, mt);
                            after(
                                env,
                                connected,
                                Box::new(move |env, result| {
                                    let hr = result[0];
                                    guest_call(env, peer, 2, vec![], move |_, _| {
                                        Outcome::Return(hr)
                                    })
                                }),
                            )
                        }),
                    )
                }),
            )
        })
    })
}

fn enum_filters(env: &mut Env<'_>, graph: u32, output: u32) -> u32 {
    if output == 0 {
        return E_POINTER;
    }
    let list = g(env, graph)
        .members
        .iter()
        .rev()
        .map(|m| m.filter)
        .collect();
    match super::create(
        env,
        &[Face::EnumFilters],
        Obj::EnumFilters {
            graph,
            list,
            position: 0,
        },
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
    // Guest sources pushing into host decoders start after every other guest
    // filter, as a graph orders Pause and Run downstream-first.
    let sources = super::dmo::upstream_filters(env, graph);
    let (mut feeding, mut rest): (Vec<u32>, Vec<u32>) = g(env, graph)
        .members
        .iter()
        .rev()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .partition(|filter| sources.contains(filter));
    rest.append(&mut feeding);
    let calls = rest
        .into_iter()
        .map(|filter| Call(filter, slot, args.clone()))
        .collect();
    chain(env, calls, Vec::new(), then)
}

fn transition_hosts(env: &mut Env<'_>, graph: u32, value: u32, start: i64, then: Then) -> Outcome {
    let mut hosts: Vec<u32> = g(env, graph)
        .members
        .iter()
        .filter_map(|m| m.host)
        .collect();
    if value != STOPPED {
        hosts.reverse();
    }
    transition_host_list(env, hosts, value, start, Vec::new(), then)
}

fn transition_host_list(
    env: &mut Env<'_>,
    mut hosts: Vec<u32>,
    value: u32,
    start: i64,
    mut results: Vec<u32>,
    then: Then,
) -> Outcome {
    if hosts.is_empty() {
        return then(env, results);
    }
    let host = hosts.remove(0);
    filter::transition(
        env,
        host,
        value,
        start,
        Box::new(move |env, hr| {
            results.push(hr);
            transition_host_list(env, hosts, value, start, results, then)
        }),
    )
}

pub(crate) fn pause(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    g(env, graph).pending_run = None;
    match g(env, graph).state {
        PAUSED => then(env, Vec::new()),
        STOPPED => {
            clock::ensure_default(env, graph, Box::new(move |env, _| paused(env, graph, then)))
        }
        _ => paused(env, graph, then),
    }
}

fn paused(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let previous = g(env, graph).state;
    if previous == STOPPED {
        let hr = video::auto_show(env, graph);
        if hr != S_OK {
            return Outcome::Return(hr);
        }
        return super::vmr::show_windows(
            env,
            graph,
            Box::new(move |env, _| pause_members(env, graph, previous, then)),
        );
    }
    pause_members(env, graph, previous, then)
}

fn pause_members(env: &mut Env<'_>, graph: u32, previous: u32, then: Then) -> Outcome {
    // Downstream first: host decoders fed by a guest source must accept the
    // samples its streaming threads push as soon as it pauses.
    transition_hosts(
        env,
        graph,
        PAUSED,
        0,
        Box::new(move |env, mut host_results| {
            each_guest_filter(
                env,
                graph,
                5,
                vec![],
                Box::new(move |env, results| {
                    let now = env.sys.clock.now_us();
                    let graph_state = g(env, graph);
                    graph_state.position = graph_state.position(now);
                    graph_state.origin = None;
                    graph_state.state = PAUSED;
                    if previous == STOPPED {
                        start_sessions(env, graph);
                    } else {
                        wake_sessions(env, graph);
                    }
                    host_results.extend(results);
                    flush_posts(env, Box::new(move |env, _| then(env, host_results)))
                }),
            )
        }),
    )
}

/// IMediaControl::Run. From Stopped the graph pauses first; when a member is
/// still pausing (S_FALSE) and none says it cannot cue, the graph reports
/// Running and S_FALSE while a worker runs the members once every one has
/// paused, as quartz does.
pub(crate) fn run(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let graph_state = g(env, graph);
    match (graph_state.state, graph_state.pending_run) {
        (RUNNING, Some(_)) => then(env, vec![S_FALSE]),
        (RUNNING, None) => then(env, Vec::new()),
        (PAUSED, _) => {
            clock::ensure_default(env, graph, Box::new(move |env, _| ran(env, graph, 0, then)))
        }
        _ => pause(
            env,
            graph,
            Box::new(move |env, paused| {
                let hr = state::combined(&paused);
                if hr == S_OK {
                    return ran(env, graph, 0, then);
                }
                if hr & 0x8000_0000 != 0 {
                    return then(env, vec![hr]);
                }
                state::cue(
                    env,
                    graph,
                    Box::new(move |env, cued| {
                        if !cued {
                            return ran(env, graph, 0, then);
                        }
                        let graph_state = g(env, graph);
                        graph_state.state = RUNNING;
                        graph_state.pending_run = Some(0);
                        stream::spawn_async_run(env, graph);
                        flush_posts(env, Box::new(move |env, _| then(env, vec![hr])))
                    }),
                )
            }),
        ),
    }
}

/// IMediaFilter::Run: every member runs with stream start `start` at once,
/// from any state.
fn run_at(env: &mut Env<'_>, graph: u32, start: u64, then: Then) -> Outcome {
    let graph_state = g(env, graph);
    if graph_state.state == RUNNING && graph_state.pending_run.is_none() {
        return then(env, Vec::new());
    }
    clock::ensure_default(
        env,
        graph,
        Box::new(move |env, _| ran(env, graph, start, then)),
    )
}

/// The asynchronous half of IMediaControl::Run, on its own thread: wait until
/// every member has paused, then run them unless the graph changed state.
pub(crate) fn async_run(env: &mut Env<'_>, graph: u32) -> Outcome {
    state::settle(
        env,
        graph,
        Box::new(move |env, hr| {
            let pending = state(env)
                .graphs
                .get_mut(&graph)
                .and_then(|g| g.pending_run.take());
            match pending {
                Some(start) if hr == S_OK => {
                    ran(env, graph, start, Box::new(|_, _| Outcome::Return(0)))
                }
                _ => Outcome::Return(0),
            }
        }),
    )
}

/// Start the stream as quartz's graph_start: a new run clears completion and
/// queued EC_COMPLETE, takes the members' stop position, counts renderers,
/// then runs every member.
fn ran(env: &mut Env<'_>, graph: u32, start: u64, then: Then) -> Outcome {
    let graph_state = g(env, graph);
    let previous = graph_state.state;
    graph_state.pending_run = None;
    graph_state.completed = 0;
    graph_state.complete_sent = false;
    graph_state.completion = None;
    graph_state.ended = false;
    graph_state.events.retain(|&(code, ..)| code != EC_COMPLETE);
    let (finished, drained) = (graph_state.finished, graph_state.events.is_empty());
    env.sys.reset_event(finished);
    if drained {
        signal_events(env, graph, false);
    }
    if previous == STOPPED {
        start_sessions(env, graph);
    }
    seeking::keep_stop(
        env,
        graph,
        Box::new(move |env| {
            count_guest_renderers(
                env,
                graph,
                Box::new(move |env| {
                    // Without a caller's start, tStart only orders samples; stream time starts at 0.
                    each_guest_filter(
                        env,
                        graph,
                        6,
                        vec![start as u32, (start >> 32) as u32],
                        Box::new(move |env, mut results| {
                            let now = env.sys.clock.now_us();
                            let graph_state = g(env, graph);
                            graph_state.origin = Some(now);
                            graph_state.state = RUNNING;
                            transition_hosts(
                                env,
                                graph,
                                RUNNING,
                                start as i64,
                                Box::new(move |env, host_results| {
                                    results.extend(host_results);
                                    if state(env).graphs.contains_key(&graph) {
                                        wake_sessions(env, graph);
                                    }
                                    flush_posts(env, Box::new(move |env, _| then(env, results)))
                                }),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}

/// A continuation taking a yes-or-no answer.
type Decided = Box<dyn FnOnce(&mut Env<'_>, bool) -> Outcome>;

fn can_query_interface(env: &Env<'_>, obj: u32) -> bool {
    let Ok(vt) = env.cpu.read_u32(obj) else {
        return false;
    };
    vt != 0 && env.cpu.read_u32(vt).unwrap_or(0) != 0
}

fn count_guest_renderers(
    env: &mut Env<'_>,
    graph: u32,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    if !state(env).graphs.contains_key(&graph) {
        return then(env);
    }
    let mut guests: Vec<u32> = g(env, graph)
        .members
        .iter()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .collect();
    guests.retain(|&filter| can_query_interface(env, filter));
    if guests.is_empty() {
        return then(env);
    }
    seeking::probe_seekers(
        env,
        graph,
        Box::new(move |env| count_next_guest_renderer(env, graph, guests.into(), 0, then)),
    )
}

fn check_no_output_pins(
    env: &mut Env<'_>,
    mut pins: VecDeque<u32>,
    has_output: bool,
    done: Decided,
) -> Outcome {
    let Some(pin) = pins.pop_front() else {
        return done(env, !has_output);
    };
    let Some(dir_buf) = alloc(env, 4) else {
        return guest_call(env, pin, 2, vec![], move |env, _| {
            check_no_output_pins(env, pins, has_output, done)
        });
    };
    guest_call(env, pin, 9, vec![dir_buf], move |env, hr| {
        let dir = env.cpu.read_u32(dir_buf).unwrap_or(0);
        free(env, dir_buf);
        let is_out = has_output || (hr == S_OK && dir == 1);
        guest_call(env, pin, 2, vec![], move |env, _| {
            check_no_output_pins(env, pins, is_out, done)
        })
    })
}

fn count_next_guest_renderer(
    env: &mut Env<'_>,
    graph: u32,
    mut guests: VecDeque<u32>,
    count: u32,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    let Some(filter) = guests.pop_front() else {
        if let Some(graph_state) = state(env).graphs.get_mut(&graph) {
            graph_state.expected = graph_state.host_expected + count;
        }
        return then(env);
    };
    let Some(scratch) = alloc(env, 20) else {
        return count_next_guest_renderer(env, graph, guests, count, then);
    };
    let _ = write_guid(env, scratch, &super::vmr::IID_IAMFILTERMISCFLAGS);
    let _ = env.cpu.write_u32(scratch + 16, 0);
    guest_call(
        env,
        filter,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let misc = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            if hr == S_OK && misc != 0 {
                free(env, scratch);
                return guest_call(env, misc, 3, vec![], move |env, flags| {
                    guest_call(env, misc, 2, vec![], move |env, _| {
                        let add = u32::from((flags & 1) != 0);
                        count_next_guest_renderer(env, graph, guests, count + add, then)
                    })
                });
            }
            let has_seek = state(env)
                .graphs
                .get(&graph)
                .and_then(|g| g.members.iter().find(|m| m.filter == filter))
                .is_some_and(|m| m.seeking != 0);
            let check_pins = move |env: &mut Env<'_>, is_seekable: bool| {
                if !is_seekable {
                    return count_next_guest_renderer(env, graph, guests, count, then);
                }
                autoplug::pins(
                    env,
                    filter,
                    Box::new(move |env, pins| {
                        check_no_output_pins(
                            env,
                            pins.into(),
                            false,
                            Box::new(move |env, no_outputs| {
                                count_next_guest_renderer(
                                    env,
                                    graph,
                                    guests,
                                    count + u32::from(no_outputs),
                                    then,
                                )
                            }),
                        )
                    }),
                )
            };
            if has_seek {
                free(env, scratch);
                return check_pins(env, true);
            }
            let _ = write_guid(env, scratch, &IID_IMEDIAPOSITION);
            let _ = env.cpu.write_u32(scratch + 16, 0);
            guest_call(
                env,
                filter,
                0,
                vec![scratch, scratch + 16],
                move |env, hr| {
                    let pos = env.cpu.read_u32(scratch + 16).unwrap_or(0);
                    free(env, scratch);
                    if hr == S_OK && pos != 0 {
                        return guest_call(env, pos, 2, vec![], move |env, _| {
                            check_pins(env, true)
                        });
                    }
                    check_pins(env, false)
                },
            )
        },
    )
}

pub(crate) fn stop(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let was_pending = g(env, graph).pending_run.take().is_some();
    match g(env, graph).state {
        STOPPED => then(env, Vec::new()),
        // Quartz pauses a running graph's members before stopping them.
        RUNNING if !was_pending => each_guest_filter(
            env,
            graph,
            5,
            vec![],
            Box::new(move |env, paused| {
                stopped(
                    env,
                    graph,
                    Box::new(move |env, mut results| {
                        results.splice(0..0, paused);
                        then(env, results)
                    }),
                )
            }),
        ),
        _ => stopped(env, graph, then),
    }
}

fn stopped(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    {
        let now = env.sys.clock.now_us();
        let graph_state = g(env, graph);
        graph_state.position = graph_state.position(now);
        graph_state.origin = None;
        graph_state.state = STOPPED;
        graph_state.stopping = true;
    }
    // Wake blocked host stages first, then stop upstream guest sources before
    // host stages and downstream guest renderers (MEDIA-DMO-4).
    super::dmo::begin_stop(env, graph);
    super::vmr::begin_stop(env, graph);
    let mut sources = super::dmo::upstream_filters(env, graph);
    sources.extend(super::vmr::upstream_filters(env, graph));
    let (feeding, rest): (Vec<u32>, Vec<u32>) = g(env, graph)
        .members
        .iter()
        .rev()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .partition(|filter| sources.contains(filter));
    let upstream_calls = feeding
        .into_iter()
        .map(|filter| Call(filter, 4, vec![]))
        .collect();
    chain(
        env,
        upstream_calls,
        Vec::new(),
        Box::new(move |env, mut results| {
            let finish_stop = move |env: &mut Env<'_>, mut results: Vec<u32>| {
                if state(env).graphs.contains_key(&graph) {
                    stop_sessions(env, graph);
                }
                let downstream_calls = rest
                    .into_iter()
                    .map(|filter| Call(filter, 4, vec![]))
                    .collect();
                chain(
                    env,
                    downstream_calls,
                    Vec::new(),
                    Box::new(move |env, more| {
                        results.extend(more);
                        join(
                            env,
                            graph,
                            Box::new(move |env, _| {
                                let Some(graph_state) = state(env).graphs.get_mut(&graph) else {
                                    return then(env, results);
                                };
                                graph_state.stopping = false;
                                // Quartz re-seeks every stream to the kept position.
                                seeking::rewind(env, graph, Box::new(move |env| then(env, results)))
                            }),
                        )
                    }),
                )
            };
            if state(env).graphs.contains_key(&graph) {
                transition_hosts(
                    env,
                    graph,
                    STOPPED,
                    0,
                    Box::new(move |env, host_results| {
                        results.extend(host_results);
                        finish_stop(env, results)
                    }),
                )
            } else {
                finish_stop(env, results)
            }
        }),
    )
}

/// Stop returns once every stream thread has left the downstream filters,
/// as quartz's parsers join theirs.
fn join(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    flush_posts(
        env,
        Box::new(move |env, _| {
            if !stream::threads_running(env, graph) {
                return then(env, Vec::new());
            }
            let idle = g(env, graph).idle;
            Outcome::block(
                Wait::Object {
                    handle: idle,
                    deadline: None,
                },
                move |env, _| then(env, Vec::new()),
            )
        }),
    )
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
    expected += super::dmo::renderers(env, graph);
    expected += super::vmr::renderers(env, graph);
    if stream::threads_running(env, graph) {
        let idle = g(env, graph).idle;
        env.sys.reset_event(idle);
    }
    let graph_state = g(env, graph);
    graph_state.host_expected = expected;
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
    let decommitting =
        code == EC_ERRORABORT && p1 == VFW_E_NOT_COMMITTED && super::dmo::decommitting(env, graph);
    let Some(graph_state) = state(env).graphs.get_mut(&graph) else {
        return;
    };
    if code == EC_ERRORABORT
        && p1 == VFW_E_NOT_COMMITTED
        && (graph_state.state == STOPPED || graph_state.stopping || decommitting)
    {
        log::debug!("DirectShow: graph {graph:#x} ignored decommit abort");
        return;
    }
    match code {
        EC_COMPLETE if graph_state.default_complete => {
            graph_state.completed += 1;
            if graph_state.expected > 0
                && graph_state.completed >= graph_state.expected
                && !graph_state.complete_sent
            {
                complete(env, graph);
            }
        }
        // The graph repaints itself; every other event reaches the
        // application unless it disabled notification.
        EC_REPAINT => log::debug!("DirectShow: graph {graph:#x} handled a repaint"),
        _ if graph_state.no_notify => {}
        _ => queue_event(env, graph, code, p1, p2),
    }
}

/// Every renderer has completed: WaitForCompletion returns, the position
/// rests at the stop, and EC_COMPLETE is queued, or with notification
/// disabled only the event handle is set.
fn complete(env: &mut Env<'_>, graph: u32) {
    let now = env.sys.clock.now_us();
    let graph_state = g(env, graph);
    graph_state.complete_sent = true;
    graph_state.completion = Some(EC_COMPLETE);
    if let Some(stop) = graph_state.stream_stop {
        graph_state.set_position(stop, now);
        graph_state.ended = true;
    }
    let finished = graph_state.finished;
    env.sys.set_event(finished);
    env.ext.get_or_default::<Playback>().completions += 1;
    if g(env, graph).no_notify {
        signal_events(env, graph, true);
    } else {
        queue_event(env, graph, EC_COMPLETE, S_OK, 0);
    }
}

/// Set or reset the graph's event and the guest's copy of it.
fn signal_events(env: &mut Env<'_>, graph: u32, set: bool) {
    let graph_state = g(env, graph);
    let (event, public_event) = (graph_state.event, graph_state.public_event);
    if set {
        env.sys.set_event(event);
    } else {
        env.sys.reset_event(event);
    }
    if let Some(event) = public_event {
        (env.ext.get::<Host>().signal)(env, event, set);
    }
}

fn queue_event(env: &mut Env<'_>, graph: u32, code: u32, p1: u32, p2: u32) {
    log::info!("DirectShow: graph {graph:#x} event {code:#x}");
    let graph_state = g(env, graph);
    graph_state.events.push_back((code, p1, p2));
    let notify = graph_state.notify;
    if matches!(code, EC_USERABORT | EC_ERRORABORT) && graph_state.completion.is_none() {
        graph_state.completion = Some(code);
        let finished = graph_state.finished;
        env.sys.set_event(finished);
    }
    signal_events(env, graph, true);
    if let Some((window, message, instance)) = notify
        && window != 0
    {
        post_message(env, window, message, 0, instance);
    }
}

/// Post through the guest-visible user32 export; this front does not own windows.
fn post_message(env: &mut Env<'_>, window: u32, message: u32, wparam: u32, lparam: u32) {
    if let Some(host) = env.ext.try_get::<Host>() {
        (host.post)(env, [window, message, wparam, lparam]);
        return;
    }
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
    if !state(env).pending_threads.is_empty() {
        let id = state(env).pending_threads.remove(0);
        let thread = env.ext.get::<Host>().thread;
        let outcome = thread(env, id);
        return after(env, outcome, Box::new(move |env, _| flush_posts(env, then)));
    }
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
            let event = g(env, graph).public_event.unwrap_or(g(env, graph).event);
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
                let graph_state = g(env, graph);
                graph_state.no_notify = flags != 0;
                // Disabling notification discards what is queued.
                if flags != 0 {
                    graph_state.events.clear();
                    signal_events(env, graph, false);
                }
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
    // A timeout clears event outputs; callers preset sentinel values and test them.
    for output in outputs {
        if output != 0 {
            let _ = env.cpu.write_u32(output, 0);
        }
    }
    let Some(graph_state) = state(env).graphs.get_mut(&graph) else {
        return Outcome::Return(E_ABORT);
    };
    if let Some((code, p1, p2)) = graph_state.events.pop_front() {
        if graph_state.events.is_empty() {
            signal_events(env, graph, false);
        }
        for (output, value) in outputs.into_iter().zip([code, p1, p2]) {
            let _ = env.cpu.write_u32(output, value);
        }
        return Outcome::Return(S_OK);
    }
    // Nothing queued: the event handle is reset, as when it was set without
    // an event (a completion with notification disabled).
    if timeout == 0 {
        signal_events(env, graph, false);
        return Outcome::Return(E_ABORT);
    }
    let handle = graph_state.event;
    let until = deadline(env, timeout);
    Outcome::block(
        Wait::Object {
            handle,
            deadline: until,
        },
        move |env, _| get_event(env, graph, outputs, 0),
    )
}

/// IMediaEvent::WaitForCompletion: the run's completion or abort, leaving
/// the event queue to GetEvent.
fn wait_completion(env: &mut Env<'_>, graph: u32, timeout: u32, output: u32) -> Outcome {
    let Some(graph_state) = state(env).graphs.get(&graph) else {
        return Outcome::Return(E_ABORT);
    };
    if let Some(code) = graph_state.completion {
        let _ = env.cpu.write_u32(output, code);
        return Outcome::Return(S_OK);
    }
    if timeout == 0 || graph_state.state != RUNNING {
        let _ = env.cpu.write_u32(output, 0);
        return Outcome::Return(E_ABORT);
    }
    let handle = graph_state.finished;
    let until = deadline(env, timeout);
    Outcome::block(
        Wait::Object {
            handle,
            deadline: until,
        },
        move |env, _| wait_completion(env, graph, 0, output),
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
        .max(state.graphs[&graph].guest_duration)
}

/// IMediaSeeking::GetCurrentPosition: the position the graph keeps, advanced
/// by the clock while running and bounded by the stop. Without a clock a
/// running graph reports 0, as quartz does.
pub(crate) fn current(env: &Env<'_>, graph: u32) -> i64 {
    let graph_state = &env.ext.get::<State>().graphs[&graph];
    match graph_state.state {
        RUNNING if graph_state.clock == 0 => 0,
        RUNNING => reached(env, graph),
        _ => graph_state.position,
    }
}

/// The running position, bounded by the stop or the host media's end.
fn reached(env: &Env<'_>, graph: u32) -> i64 {
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

/// Guest sources feeding host decoders seek themselves and flush downstream;
/// pass them the absolute positions the graph just took.
fn seek_guests(
    env: &mut Env<'_>,
    graph: u32,
    position: Option<i64>,
    stop: Option<i64>,
    then: Then,
) -> Outcome {
    const ABSOLUTE: u32 = 1;
    let seekers = super::dmo::upstream_seekers(env, graph);
    if seekers.is_empty() || (position.is_none() && stop.is_none()) {
        return then(env, Vec::new());
    }
    let Some(scratch) = alloc(env, 16) else {
        return then(env, Vec::new());
    };
    write_i64(env, scratch, position.unwrap_or(0));
    write_i64(env, scratch + 8, stop.unwrap_or(0));
    let flags = |value: Option<i64>| if value.is_some() { ABSOLUTE } else { 0 };
    let calls = seekers
        .into_iter()
        .map(|seeker| {
            Call(
                seeker,
                14,
                vec![scratch, flags(position), scratch + 8, flags(stop)],
            )
        })
        .collect();
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            free(env, scratch);
            then(env, results)
        }),
    )
}

fn resolve_position(current: i64, value: i64, flags: u32) -> Option<i64> {
    match flags & 3 {
        0 => None,
        1 => Some(value),
        2 | 3 => Some(current + value),
        _ => None,
    }
}

/// Whether a host audio renderer in the graph answers IBasicAudio.
fn host_audio(env: &Env<'_>, graph: u32) -> bool {
    let state = env.ext.get::<State>();
    state.graphs[&graph]
        .members
        .iter()
        .filter_map(|m| m.host)
        .any(|id| state.filters[&id].role == filter::Role::AudioRenderer)
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
