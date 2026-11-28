//! UI presents the native game view. The execution worker owns the guest machine.
//! Completed raw frames and input events cross bounded ownership channels.
use crate::{performance, window};
#[cfg(not(any(host_view, feature = "gpu")))]
use adv_runtime::FrameConsumer;
#[cfg(not(any(host_view, feature = "gpu")))]
use adv_runtime::FrameProducer;
use adv_runtime::{DialogSet, MenuBar, MenuKey};
use adv_sync::{WakeEvent, input::KeyState, spsc};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};
/// Owner of the native menu and dialog projection the sinks are given to.
#[cfg(target_os = "macos")]
type SinkTarget = window::Projection;

enum Request {
    Hide,
    Minimize,
    OpenLink(String),
}
// Native statistics move each frame; boxing them would allocate per frame.
#[allow(clippy::large_enum_variant)]
enum Delivery {
    Input(window::Event),
    #[cfg(mobile)]
    AndroidInput(crate::window::android_epoch::Stamp, window::Event),
    Present(u64, Duration, Instant),
    Pump(Duration, Instant),
    #[cfg(host_view)]
    Native(performance::NativePresentation),
}
const INPUT_CAPACITY: usize = 4096;
const METRIC_LIMIT: usize = 128;
const RESERVED_INPUT: usize = INPUT_CAPACITY - METRIC_LIMIT;
pub struct Worker {
    #[cfg(mobile)]
    input_epoch: Arc<window::android_epoch::Generation>,
    #[cfg(mobile)]
    keys: Arc<KeyState>,

    #[cfg(any(host_view, feature = "gpu"))]
    pub(crate) display: Option<crate::display::Startup>,
    /// The host frame clock's vsync grid, handed to guest refresh pacing.
    #[cfg(host_view)]
    pub(crate) vsync: Option<Arc<adv_sync::vsync::Vsync>>,
    incoming: spsc::Consumer<Delivery>,
    requests: spsc::Producer<Request>,
    #[cfg(not(any(host_view, feature = "gpu")))]
    frames: Option<FrameProducer>,
    menus: adv_sync::mailbox::Producer<MenuBar>,
    menu_key: Option<MenuKey>,
    dialogs: adv_sync::mailbox::Producer<DialogSet>,
    dialog_set: DialogSet,
    pub ui_wake: WakeEvent,
    stopped: Arc<AtomicBool>,
    metrics_dropped: Arc<AtomicU64>,
}
pub struct Ui {
    #[cfg(mobile)]
    input_epoch: Arc<window::android_epoch::Generation>,
    #[cfg(mobile)]
    paused: bool,
    #[cfg(host_view)]
    frame_times: std::collections::VecDeque<Instant>,
    /// Whether the game has shown a frame yet; until then it is loading.
    #[cfg(host_view)]
    presented: bool,
    /// Called once the guest first shows a frame or a dialog.
    #[cfg(host_view)]
    on_shown: Option<Box<dyn FnOnce()>>,

    #[cfg(host_view)]
    #[cfg(host_view)]
    pub(crate) menu_changed: bool,
    #[cfg(host_view)]
    pub(crate) dialog_changed: bool,
    #[cfg(host_view)]
    repaint: bool,
    events: Rc<RefCell<spsc::Producer<Delivery>>>,
    requests: spsc::Consumer<Request>,
    #[cfg(not(any(host_view, feature = "gpu")))]
    frames: Option<FrameConsumer>,
    menus: adv_sync::mailbox::Consumer<MenuBar>,
    dialogs: adv_sync::mailbox::Consumer<DialogSet>,

    #[cfg(any(host_view, feature = "gpu"))]
    native: Option<crate::display::Ui>,
    keys: Arc<KeyState>,
    execution_wake: WakeEvent,
    wake: WakeEvent,
    stopped: Arc<AtomicBool>,
    metrics_dropped: Arc<AtomicU64>,
}
pub fn channel(wake: WakeEvent, keys: Arc<KeyState>) -> Result<(Worker, Ui), String> {
    let ui_wake = WakeEvent::new().map_err(|e| e.to_string())?;
    let (events, incoming) = spsc::channel(INPUT_CAPACITY);
    let (requests_tx, requests_rx) = spsc::channel(32);
    #[cfg(not(any(host_view, feature = "gpu")))]
    let (frames_tx, _frames_rx) = adv_runtime::frame_mailbox();
    let (menus_tx, menus_rx) =
        adv_sync::mailbox::channel(std::array::from_fn(|_| MenuBar::default()));
    let (dialogs_tx, dialogs_rx) =
        adv_sync::mailbox::channel(std::array::from_fn(|_| DialogSet::default()));
    let stopped = Arc::new(AtomicBool::new(false));
    let metrics_dropped = Arc::new(AtomicU64::new(0));
    #[cfg(mobile)]
    let input_epoch = Arc::new(window::android_epoch::Generation::default());
    Ok((
        Worker {
            #[cfg(mobile)]
            input_epoch: input_epoch.clone(),
            #[cfg(mobile)]
            keys: keys.clone(),

            #[cfg(any(host_view, feature = "gpu"))]
            display: None,
            #[cfg(host_view)]
            vsync: None,
            incoming,
            requests: requests_tx,
            #[cfg(not(any(host_view, feature = "gpu")))]
            frames: Some(frames_tx),
            menus: menus_tx,
            menu_key: None,
            dialogs: dialogs_tx,
            dialog_set: DialogSet::default(),
            ui_wake: ui_wake.clone(),
            stopped: stopped.clone(),
            metrics_dropped: metrics_dropped.clone(),
        },
        Ui {
            #[cfg(mobile)]
            input_epoch,
            #[cfg(mobile)]
            paused: false,
            #[cfg(host_view)]
            frame_times: std::collections::VecDeque::new(),
            #[cfg(host_view)]
            presented: false,
            #[cfg(host_view)]
            on_shown: None,

            #[cfg(host_view)]
            #[cfg(host_view)]
            menu_changed: false,
            #[cfg(host_view)]
            dialog_changed: false,
            #[cfg(host_view)]
            repaint: false,
            events: Rc::new(RefCell::new(events)),
            requests: requests_rx,
            #[cfg(not(any(host_view, feature = "gpu")))]
            frames: Some(_frames_rx),
            menus: menus_rx,
            dialogs: dialogs_rx,
            #[cfg(host_view)]
            native: None,
            keys,
            execution_wake: wake,
            wake: ui_wake,
            stopped,
            metrics_dropped,
        },
    ))
}
impl Worker {
    pub fn publish_dialogs(&mut self, dialogs: DialogSet) {
        if dialogs == self.dialog_set {
            return;
        }
        self.dialog_set = dialogs.clone();
        self.dialogs.publish(move |slot| *slot = dialogs);
        self.ui_wake.signal();
    }
    pub fn needs_menu(&self, key: MenuKey) -> bool {
        self.menu_key != Some(key)
    }
    pub fn publish_menu(&mut self, menu: MenuBar) {
        self.menu_key = Some(menu.key);
        self.menus.publish(move |slot| *slot = menu);
        self.ui_wake.signal();
    }
    #[cfg(not(any(host_view, feature = "gpu")))]
    pub fn take_frames(&mut self) -> FrameProducer {
        self.frames.take().expect("frame producer installed once")
    }
    #[cfg(test)]
    pub fn pump(&mut self, performance: &mut performance::Performance) -> Vec<window::Event> {
        std::iter::from_fn(|| self.next_input(performance)).collect()
    }
    pub(crate) fn next_input(
        &mut self,
        performance: &mut performance::Performance,
    ) -> Option<window::Event> {
        while let Some(delivery) = self.incoming.pop() {
            match delivery {
                Delivery::Input(event) => return Some(event),
                #[cfg(mobile)]
                Delivery::AndroidInput(stamp, event) => {
                    if self.input_epoch.current(stamp, &self.keys) {
                        return Some(event);
                    }
                }
                Delivery::Present(serial, elapsed, at) => performance.present(serial, elapsed, at),
                Delivery::Pump(elapsed, at) => performance.pump(elapsed, at),
                #[cfg(host_view)]
                Delivery::Native(stats) => performance.native(stats),
            }
        }
        performance.set_ui_metrics_dropped(self.metrics_dropped.load(Ordering::Relaxed));
        None
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn hide(&mut self) {
        let _ = self.requests.push(Request::Hide);
        self.ui_wake.signal();
    }
    /// The guest minimized the displayed window.
    pub fn minimize(&mut self) {
        let _ = self.requests.push(Request::Minimize);
        self.ui_wake.signal();
    }
    /// The guest asked to open a web or mail address.
    pub fn open_link(&mut self, url: String) {
        let _ = self.requests.push(Request::OpenLink(url));
        self.ui_wake.signal();
    }
}
impl Ui {
    /// The frame clock fired: one display refresh passed.
    #[cfg(host_view)]
    pub(crate) fn refreshed(&self) {
        if let Some(native) = &self.native {
            native.refreshed();
        }
    }
    #[cfg(host_view)]
    pub(crate) fn fps(&self) -> f64 {
        self.frame_times
            .iter()
            .filter(|at| at.elapsed() < Duration::from_secs(1))
            .count() as f64
    }

    #[cfg(host_view)]
    pub(crate) fn presented(&self) -> bool {
        self.presented
    }

    #[cfg(host_view)]
    pub(crate) fn observe_shown(&mut self, observer: Box<dyn FnOnce()>) {
        self.on_shown = Some(observer);
        if self.presented || !self.dialogs.current().dialogs.is_empty() {
            self.shown();
        }
    }

    #[cfg(host_view)]
    fn shown(&mut self) {
        if let Some(observer) = self.on_shown.take() {
            observer();
        }
    }

    /// Milliseconds between the presents of the last second, oldest first.
    #[cfg(host_view)]
    pub(crate) fn frame_intervals(&self) -> Vec<f32> {
        let recent: Vec<_> = self
            .frame_times
            .iter()
            .filter(|at| at.elapsed() < Duration::from_secs(1))
            .collect();
        recent
            .windows(2)
            .map(|pair| pair[1].duration_since(*pair[0]).as_secs_f32() * 1000.0)
            .collect()
    }

    #[cfg(host_view)]
    pub(crate) fn report_settings(&self) -> Option<(adv_report::Upscaler, adv_report::Scaling)> {
        self.native
            .as_ref()
            .map(crate::display::Ui::report_settings)
    }
    #[cfg(host_view)]
    pub(crate) fn set_scaling(&mut self, value: crate::Scaling) {
        if let Some(native) = &mut self.native {
            native.set_scaling(value);
        }
        self.repaint = true;
        self.wake.signal();
    }
    #[cfg(host_view)]
    pub(crate) fn set_upscaler(&mut self, value: crate::Upscaler) {
        if let Some(native) = &mut self.native {
            native.set_upscaler(value);
        }
        self.repaint = true;
        self.wake.signal();
    }

    #[cfg(any(host_view, feature = "gpu"))]
    pub(crate) fn display(&mut self, display: crate::display::Ui) {
        self.native = Some(display);
    }
    #[cfg(mobile)]
    pub(crate) fn trim_memory(&mut self) {
        if let Some(native) = &mut self.native {
            native.trim();
        }
    }
    #[cfg(mobile)]
    pub(crate) fn pause_input(&mut self, paused: bool) {
        if paused && !self.paused {
            self.input_epoch.invalidate();
            self.keys.release_all();
        }
        self.paused = paused;
    }
    #[cfg(target_os = "android")]
    pub(crate) unsafe fn replace_window(
        &mut self,
        window: Option<std::ptr::NonNull<ndk_sys::ANativeWindow>>,
    ) -> Result<(), String> {
        if let Some(native) = &mut self.native {
            unsafe {
                native.replace_window(window)?;
            }
        }
        self.repaint = window.is_some();
        self.wake.signal();
        Ok(())
    }
    fn deliver(&mut self, event: Delivery) -> Result<(), String> {
        #[cfg(mobile)]
        let event = match event {
            Delivery::Input(
                event @ (window::Event::Input { .. }
                | window::Event::WindowInput { .. }
                | window::Event::Menu(_)
                | window::Event::Dialog(_)
                | window::Event::Text(..)),
            ) => {
                if self.paused {
                    return Ok(());
                }
                let Some(stamp) = self.input_epoch.stamp(&event, &self.keys) else {
                    return Ok(());
                };
                Delivery::AndroidInput(stamp, event)
            }
            other => other,
        };
        self.events
            .borrow_mut()
            .push(event)
            .map_err(|_| "UI input channel capacity exceeded")?;
        self.execution_wake.signal_frontend();
        Ok(())
    }
    /// Telemetry never wakes execution and can occupy at most 128 queue slots.
    /// Saturation is counted; key releases and close/focus events keep the reserve.
    fn metric(&mut self, event: Delivery) {
        debug_assert!(!matches!(event, Delivery::Input(_)));
        let mut events = self.events.borrow_mut();
        if events.available() <= RESERVED_INPUT || events.push(event).is_err() {
            self.metrics_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn pump_events(
        &mut self,
        events: Vec<window::Event>,
        started: Instant,
    ) -> Result<bool, String> {
        let mut repaint = false;
        let mut had_input = false;
        for event in events {
            match &event {
                window::Event::Input {
                    message,
                    wparam,
                    lparam,
                    ..
                } => {
                    #[cfg(mobile)]
                    let update =
                        !(*wparam == 0x11 && matches!(*message, 0x100 | 0x101 | 0x104 | 0x105));
                    #[cfg(not(mobile))]
                    let update = true;
                    if update {
                        update_keys(&self.keys, *message, *wparam, *lparam);
                    }
                }
                window::Event::WindowInput {
                    message,
                    wparam,
                    lparam,
                    ..
                } => update_keys(&self.keys, *message, *wparam, *lparam),
                window::Event::FocusLost | window::Event::Close => self.keys.release_all(),
                window::Event::Repaint => {
                    repaint = true;
                    continue;
                }
                #[cfg(host_view)]
                window::Event::AudioSuspended(_)
                | window::Event::Volume(_)
                | window::Event::Paused(_)
                | window::Event::Disc(..)
                | window::Event::Text(..) => {}
                #[cfg(host_view)]
                window::Event::ForceClose => {
                    self.stopped.store(true, Ordering::Release);
                    self.execution_wake.signal_frontend();
                    continue;
                }
                window::Event::Menu(_)
                | window::Event::Dialog(_)
                | window::Event::ReportSnapshot(_) => {}
            }
            had_input = true;
            self.deliver(Delivery::Input(event))?;
        }
        if had_input {
            let completed = Instant::now();
            self.metric(Delivery::Pump(
                completed.saturating_duration_since(started),
                completed,
            ));
        }
        Ok(repaint)
    }
    #[cfg(host_view)]
    pub(crate) fn post_text(&mut self, text: &str) -> usize {
        #[cfg(mobile)]
        if self.paused {
            return 0;
        }
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        if self.input(window::Event::Text(text.into(), reply)).is_err() {
            return 0;
        }
        result.recv().unwrap_or(0)
    }
    #[cfg(host_view)]
    pub(crate) fn input(&mut self, event: window::Event) -> Result<(), String> {
        self.repaint |= self.pump_events(vec![event], Instant::now())?;
        Ok(())
    }
    #[cfg(host_view)]
    pub(crate) fn tick(&mut self, window: &window::GameView) -> Result<Option<Instant>, String> {
        while let Some(request) = self.requests.pop() {
            match request {
                Request::Hide => window.hide(),
                Request::Minimize => window.minimize(),
                Request::OpenLink(url) => window.open_link(url),
            }
        }
        self.menu_changed |= self.menus.update();
        let had_dialogs = !self.dialogs.current().dialogs.is_empty();
        if self.dialogs.update() {
            self.dialog_changed = true;
            if !had_dialogs && !self.dialogs.current().dialogs.is_empty() {
                // Physical focus moves to the native controls; their key
                // releases no longer pass through the canvas procedure.
                self.keys.release_all();
            }
        }
        window.presentation(self.menus.current(), self.dialogs.current())?;
        if !self.dialogs.current().dialogs.is_empty() {
            self.shown();
        }
        let started = Instant::now();
        let events = window.take_events();
        self.repaint |= self.pump_events(events, started)?;

        if self.native.is_some() {
            let dimensions = window.native_client_size()?;
            let scale = window.native_scale();
            while let Some(feedback) = self.native.as_mut().unwrap().poll()? {
                let completed = match feedback {
                    crate::display::Feedback::Statistics(stats) => {
                        self.metric(Delivery::Native(stats));
                        continue;
                    }
                    crate::display::Feedback::Attached => {
                        window.frame_presented();
                        continue;
                    }
                    crate::display::Feedback::Presented(completed) => completed,
                };
                window.frame_presented();
                self.metric(Delivery::Native(completed.stats));
                window.native_canvas(completed.canvas)?;
                window.place(Some(completed.placement));
                window.place_windows(completed.windows);
                window.set_video_cursor_regions(completed.cursor_regions);
                let now = Instant::now();
                while self
                    .frame_times
                    .front()
                    .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1))
                {
                    self.frame_times.pop_front();
                }
                self.frame_times.push_back(now);
                self.presented = true;
                self.shown();
                self.metric(Delivery::Present(
                    completed.serial,
                    completed.elapsed,
                    completed.at,
                ));
            }
            self.native.as_mut().unwrap().update(dimensions, scale);
            self.repaint = false;
        }
        self.deadline(window)
    }
    #[cfg(host_view)]
    pub(crate) fn deadline(&self, window: &window::GameView) -> Result<Option<Instant>, String> {
        let refresh: Option<Instant> = {
            let _ = window;
            None
        };
        // Touch gestures time long presses and click holds (AND-APP-14).
        #[cfg(mobile)]
        let refresh = match (refresh, window.input_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok(refresh)
    }
    #[cfg(host_view)]
    pub(crate) fn menu_snapshot(&self) -> &MenuBar {
        self.menus.current()
    }
    #[cfg(host_view)]
    pub(crate) fn dialog_snapshot(&self) -> &DialogSet {
        self.dialogs.current()
    }
    #[cfg(not(mobile))]
    pub(crate) fn install_sinks(&self, window: &mut SinkTarget) {
        // Menu modal dispatch still runs on this UI thread. Its callbacks can
        // enqueue events immediately without waiting for the outer paint loop.
        let events = self.events.clone();
        let execution_wake = self.execution_wake.clone();
        window.set_menu_event_sink(Box::new(move |event| {
            events
                .borrow_mut()
                .push(Delivery::Input(window::Event::Menu(event)))
                .map_err(|_| "UI menu input channel capacity exceeded".to_owned())?;
            execution_wake.signal_frontend();
            Ok(())
        }));
        let events = self.events.clone();
        let execution_wake = self.execution_wake.clone();
        window.set_dialog_event_sink(Box::new(move |event| {
            events
                .borrow_mut()
                .push(Delivery::Input(window::Event::Dialog(event)))
                .map_err(|_| "UI dialog input channel capacity exceeded".to_owned())?;
            execution_wake.signal_frontend();
            Ok(())
        }));
    }
    pub fn wake(&self) -> WakeEvent {
        self.wake.clone()
    }
}
#[cfg(test)]
fn refresh_deadline(
    next: Instant,
    in_flight: bool,
    dirty: bool,
    dimensions: (u32, u32),
) -> Option<Instant> {
    (!in_flight && dirty && dimensions.0 != 0 && dimensions.1 != 0).then_some(next)
}
/// Tracks Android display refreshes so presentation follows HOST-SES-8:
/// - slower or ~60 Hz displays count every vsync,
/// - displays within 1% of a whole multiple of 60 Hz count every k-th vsync,
/// - other faster displays snap rational 60 Hz cadence to the nearest host
///   vsync (consistent with GFX-PACE-1) without repaying missed frames,
/// - an unknown period (`<= 0`) enforces a 90% of 1/60 s floor to avoid
///   120 Hz half-period boundary jitter.
#[cfg(any(target_os = "android", test))]
#[derive(Default)]
pub(crate) struct RefreshPacer {
    last_counted: Option<i64>,
    cadence_origin: i64,
    cadence_step: u64,
}
#[cfg(any(target_os = "android", test))]
impl RefreshPacer {
    const SIXTY_NS: i64 = 16_666_667;
    const UNKNOWN_FLOOR_NS: i64 = Self::SIXTY_NS - Self::SIXTY_NS / 10;

    fn anchor(&mut self, frame_ns: i64) -> bool {
        self.last_counted = Some(frame_ns);
        self.cadence_origin = frame_ns;
        self.cadence_step = 0;
        true
    }

    /// Whether the vsync at `frame_ns` counts as a presentation refresh.
    pub(crate) fn counts(&mut self, frame_ns: i64, period_ns: i64) -> bool {
        let Some(last) = self.last_counted else {
            return self.anchor(frame_ns);
        };
        let elapsed = frame_ns.saturating_sub(last);
        if period_ns <= 0 {
            return if elapsed >= Self::UNKNOWN_FLOOR_NS {
                self.anchor(frame_ns)
            } else {
                false
            };
        }
        let rate = 1e9 / period_ns as f64;
        if rate <= 60.0 * 1.01 {
            return self.anchor(frame_ns);
        }
        let whole = (rate / 60.0).round();
        if whole >= 2.0 && (rate - whole * 60.0).abs() <= 0.01 * rate {
            let every = whole as i64;
            return if elapsed >= every * period_ns - period_ns / 2 {
                self.anchor(frame_ns)
            } else {
                false
            };
        }
        let next_step = self.cadence_step + 1;
        let ideal =
            i128::from(self.cadence_origin) + (i128::from(next_step) * 1_000_000_000 + 30) / 60;
        let next_ideal =
            i128::from(self.cadence_origin) + (i128::from(next_step + 1) * 1_000_000_000 + 30) / 60;
        let snap_margin = i128::from(period_ns / 2);
        let frame = i128::from(frame_ns);
        if frame < ideal - snap_margin {
            return false;
        }
        if frame >= next_ideal - snap_margin {
            return self.anchor(frame_ns);
        }
        self.last_counted = Some(frame_ns);
        self.cadence_step = next_step;
        true
    }
}
/// Tracks the display's vsync period from `AChoreographer` refresh-rate
/// callbacks (API 30+) or consecutive un-gapped frame callbacks (API 29).
#[cfg(any(target_os = "android", test))]
#[derive(Default)]
pub(crate) struct PeriodEstimator {
    reported: Option<i64>,
    inferred: Option<i64>,
    last_frame_ns: Option<i64>,
    posted_at_ns: Option<i64>,
    candidate: Option<(i64, u8)>,
}
#[cfg(any(target_os = "android", test))]
impl PeriodEstimator {
    /// Bounds covering ~285 Hz..~28.6 Hz (240 Hz..30 Hz with jitter margin).
    const MIN_PERIOD_NS: i64 = 3_500_000;
    const MAX_PERIOD_NS: i64 = 35_000_000;
    /// Consecutive matching callbacks required to establish an initial or faster period.
    const CONSISTENT_SAMPLES: u8 = 2;
    /// Consecutive matching callbacks required to switch to a non-multiple slower period.
    const SLOWER_SAMPLES: u8 = 4;
    /// Consecutive matching callbacks required to switch to an integer multiple of the
    /// current period (e.g. genuine 120 Hz -> 60 Hz switch vs missed Choreographer callbacks).
    const SLOWER_MULTIPLE_SAMPLES: u8 = 8;

    pub(crate) fn set_reported(&mut self, period_ns: i64) {
        if period_ns > 0 {
            self.reported = Some(period_ns);
        }
    }

    pub(crate) fn on_post(&mut self, now_ns: i64) {
        self.posted_at_ns = Some(now_ns);
    }

    fn periods_match(a: i64, b: i64) -> bool {
        (a - b).abs() * 10 <= a.min(b)
    }

    fn is_integer_multiple(delta: i64, base: i64) -> bool {
        if delta <= base || base <= 0 {
            return false;
        }
        let mult = (delta + base / 2) / base;
        mult >= 2 && (delta - mult * base).abs() * 10 <= mult * base
    }

    fn record_candidate(&mut self, delta: i64, required: u8) -> Option<i64> {
        match self.candidate {
            Some((cand, count)) if Self::periods_match(delta, cand) => {
                let next_count = count + 1;
                let avg = (cand * i64::from(count) + delta) / i64::from(next_count);
                if next_count >= required {
                    self.candidate = None;
                    Some(avg)
                } else {
                    self.candidate = Some((avg, next_count));
                    None
                }
            }
            _ => {
                self.candidate = Some((delta, 1));
                None
            }
        }
    }

    pub(crate) fn on_frame(&mut self, frame_ns: i64) -> Option<i64> {
        let prev = self.last_frame_ns.replace(frame_ns);
        let posted = self.posted_at_ns.take();
        if let Some(period) = self.reported {
            return Some(period);
        }
        let adjacent = prev.zip(posted).and_then(|(prev, posted)| {
            let delta = frame_ns.saturating_sub(prev);
            let post_delay = posted.saturating_sub(prev);
            let tight_post = self
                .inferred
                .is_none_or(|cur| delta <= cur || post_delay < cur / 2);
            ((Self::MIN_PERIOD_NS..=Self::MAX_PERIOD_NS).contains(&delta)
                && posted >= prev.saturating_sub(2_000_000)
                && post_delay < delta / 4
                && tight_post)
                .then_some(delta)
        });
        let Some(delta) = adjacent else {
            // Idle gaps, missing posts, or out-of-range deltas break any
            // consecutive candidate run so isolated missed callbacks never accumulate.
            self.candidate = None;
            return self.inferred;
        };
        match self.inferred {
            None => {
                if let Some(confirmed) = self.record_candidate(delta, Self::CONSISTENT_SAMPLES) {
                    self.inferred = Some(confirmed);
                }
            }
            Some(cur) if Self::periods_match(delta, cur) => {
                self.inferred = Some((cur * 7 + delta) / 8);
                self.candidate = None;
            }
            Some(cur) if delta < cur => {
                // Shrink promptly once a shorter consistent period is observed.
                if let Some(faster) = self.record_candidate(delta, Self::CONSISTENT_SAMPLES) {
                    self.inferred = Some(faster);
                }
            }
            Some(cur) => {
                let required = if Self::is_integer_multiple(delta, cur) {
                    Self::SLOWER_MULTIPLE_SAMPLES
                } else {
                    Self::SLOWER_SAMPLES
                };
                if let Some(slower) = self.record_candidate(delta, required) {
                    self.inferred = Some(slower);
                }
            }
        }
        self.inferred
    }
}
#[cfg(any(mobile, test))]
pub(crate) fn request_vsync(next: Option<Instant>, pending: bool) -> bool {
    !pending && next.is_some_and(|next| next <= Instant::now())
}
impl Drop for Ui {
    fn drop(&mut self) {
        self.keys.release_all();
        self.stopped.store(true, Ordering::Release);
        self.execution_wake.signal_frontend();
    }
}
pub fn update_keys(keys: &KeyState, message: u32, key: u32, lparam: u32) {
    match message {
        0x100 | 0x101 | 0x104 | 0x105 => {
            let pressed = message & 1 == 0;
            keys.set(key, pressed);
            if let Some(side) = adv_os::keyboard::side_virtual_key(key, lparam) {
                keys.set(side, pressed);
            }
        }
        0x201 => keys.set(1, true),
        0x202 => keys.set(1, false),
        0x204 => keys.set(2, true),
        0x205 => keys.set(2, false),
        0x207 => keys.set(4, true),
        0x208 => keys.set(4, false),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: HOST-SES-2, AND-APP-10
    #[cfg(host_view)]
    #[test]
    fn tick_deadline_is_only_armed_for_a_visible_pending_refresh() {
        let next = Instant::now() + Duration::from_millis(16);
        assert_eq!(refresh_deadline(next, false, true, (320, 240)), Some(next));
        assert_eq!(refresh_deadline(next, false, false, (320, 240)), None);
        assert_eq!(refresh_deadline(next, true, true, (320, 240)), None);
        assert_eq!(refresh_deadline(next, false, true, (0, 0)), None);
        let overdue = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            refresh_deadline(overdue, false, true, (320, 240)),
            Some(overdue)
        );
    }
    // spec: HOST-SES-8
    #[test]
    fn presentation_refreshes_stay_at_sixty_hertz_on_faster_panels() {
        let count_jittered = |period: i64, ticks: i64| {
            let mut pacer = RefreshPacer::default();
            let mut intervals = Vec::new();
            let mut last = None;
            for n in 1..=ticks {
                let at = n * period + if n % 2 == 0 { 300_000 } else { -300_000 };
                if pacer.counts(at, period) {
                    if let Some(prev) = last {
                        intervals.push(at - prev);
                    }
                    last = Some(at);
                }
            }
            (intervals.len() + usize::from(last.is_some()), intervals)
        };
        // Slower, 60 Hz, and whole-multiple panels pace strictly at 60 Hz (or panel rate).
        assert_eq!(count_jittered(20_000_000, 50).0, 50);
        assert_eq!(count_jittered(16_666_667, 60).0, 60);
        let (hz120, iv120) = count_jittered(8_333_333, 120);
        assert_eq!(hz120, 60);
        assert!(iv120.iter().all(|&d| (d - 16_666_666).abs() < 1_000_000));
        let (hz180, iv180) = count_jittered(5_555_556, 180);
        assert_eq!(hz180, 60);
        assert!(iv180.iter().all(|&d| (d - 16_666_668).abs() < 1_000_000));
        let (hz240, iv240) = count_jittered(4_166_667, 240);
        assert_eq!(hz240, 60);
        assert!(iv240.iter().all(|&d| (d - 16_666_668).abs() < 1_000_000));

        // Non-integer faster panels snap rational 60 Hz cadence to nearest vsyncs.
        for (period, ticks) in [(11_111_111, 90), (6_944_444, 144), (6_060_606, 165)] {
            let (total, intervals) = count_jittered(period, ticks);
            assert_eq!(total, 60, "period={period} ticks={ticks}");
            let min_iv = *intervals.iter().min().unwrap();
            let max_iv = *intervals.iter().max().unwrap();
            assert!(
                min_iv >= period - 700_000 && max_iv <= 3 * period + 700_000,
                "period={period} min={min_iv} max={max_iv}"
            );
        }

        // Unknown period (API 29 before measurement): 120 Hz never jitters on a 1 ns boundary.
        let mut unknown = RefreshPacer::default();
        assert!(unknown.counts(0, 0));
        assert!(!unknown.counts(8_333_333, 0));
        assert!(!unknown.counts(8_333_334, 0));
        assert!(!unknown.counts(8_334_000, 0));
        assert!(unknown.counts(16_666_667, 0));

        // Idle gaps re-anchor instead of repaying missed 60 Hz frames.
        let mut idle = RefreshPacer::default();
        assert!(idle.counts(0, 6_944_444));
        assert!(idle.counts(500_000_000, 6_944_444));
        assert!(!idle.counts(506_944_444, 6_944_444));
        assert!(idle.counts(513_888_888, 6_944_444));
    }
    // spec: AND-APP-10
    #[test]
    fn api29_period_estimator_infers_adjacent_display_period_and_rejects_idle_gaps() {
        // Adjacent 120 Hz callbacks infer ~8.33 ms once confirmed by consecutive samples.
        let mut est = PeriodEstimator::default();
        est.on_post(0);
        assert_eq!(est.on_frame(1_000_000), None);
        est.on_post(1_500_000);
        assert_eq!(est.on_frame(9_333_333), None);
        est.on_post(9_800_000);
        assert_eq!(est.on_frame(17_666_666), Some(8_333_333));
        // An idle gap (posted 9 ms after previous frame) must not overwrite 120 Hz with 60 Hz.
        est.on_post(26_666_666);
        assert_eq!(est.on_frame(34_333_333), Some(8_333_333));

        // Reported API 30+ refresh-rate callbacks take precedence over inferred periods.
        est.set_reported(11_111_111);
        est.on_post(34_800_000);
        assert_eq!(est.on_frame(42_666_666), Some(11_111_111));
    }
    // spec: AND-APP-10
    #[test]
    fn api29_period_estimator_rejects_missed_callbacks_and_adapts_to_rate_switches() {
        let step = |est: &mut PeriodEstimator, t: &mut i64, delta: i64| -> Option<i64> {
            est.on_post(*t + 500_000);
            *t += delta;
            est.on_frame(*t)
        };
        for period in [16_666_667i64, 11_111_111, 8_333_333, 6_944_444] {
            // A dropped first callback (2*period) must not be adopted as the panel period.
            let mut est = PeriodEstimator::default();
            let mut t = 1_000_000i64;
            est.on_post(0);
            assert_eq!(est.on_frame(t), None);
            assert_eq!(step(&mut est, &mut t, period * 2), None);
            assert_eq!(step(&mut est, &mut t, period), None);
            let confirmed = step(&mut est, &mut t, period).unwrap();
            assert!(
                (confirmed - period).abs() <= 2,
                "period={period} confirmed={confirmed}"
            );

            // Single 2x and 3x missed-callback gaps, and repeated isolated gaps separated
            // by idle pauses or normal callbacks, must not alter the estimated period.
            for mult in [2i64, 3] {
                for _ in 0..6 {
                    let got = step(&mut est, &mut t, period * mult).unwrap();
                    assert!(
                        (got - period).abs() <= 10,
                        "period={period} mult={mult} got={got}"
                    );
                    // Interleave an idle pause so isolated misses cannot accumulate.
                    est.on_post(t + period);
                    t += period * 2;
                    let after_idle = est.on_frame(t).unwrap();
                    assert!(
                        (after_idle - period).abs() <= 10,
                        "period={period} after_idle={after_idle}"
                    );
                }
                let recovered = step(&mut est, &mut t, period).unwrap();
                assert!(
                    (recovered - period).abs() <= 10,
                    "period={period} recovered={recovered}"
                );
            }
        }

        // Genuine sustained 120 Hz -> 60 Hz switch adapts cleanly, then shrinks back to 120 Hz.
        let mut est = PeriodEstimator::default();
        let mut t = 1_000_000i64;
        est.on_post(0);
        est.on_frame(t);
        step(&mut est, &mut t, 8_333_333);
        assert_eq!(step(&mut est, &mut t, 8_333_333), Some(8_333_333));
        // Brief 3-frame burst of dropped callbacks stays at 120 Hz.
        for _ in 0..3 {
            assert_eq!(step(&mut est, &mut t, 16_666_667), Some(8_333_333));
        }
        assert_eq!(step(&mut est, &mut t, 8_333_333), Some(8_333_333));
        // Sustained 60 Hz across 8 consecutive callbacks switches cleanly.
        for i in 1..=8 {
            let got = step(&mut est, &mut t, 16_666_667).unwrap();
            if i < 8 {
                assert_eq!(got, 8_333_333, "switched too early at sample {i}");
            } else {
                assert_eq!(got, 16_666_667, "failed to adopt sustained 60 Hz");
            }
        }
        // Switching back up to 120 Hz shrinks promptly on 2 consistent callbacks.
        assert_eq!(step(&mut est, &mut t, 8_333_333), Some(16_666_667));
        assert_eq!(step(&mut est, &mut t, 8_333_333), Some(8_333_333));

        // Non-multiple sustained switch (90 Hz -> 60 Hz) adapts in 4 consecutive callbacks.
        let mut est90 = PeriodEstimator::default();
        let mut t90 = 1_000_000i64;
        est90.on_post(0);
        est90.on_frame(t90);
        step(&mut est90, &mut t90, 11_111_111);
        assert_eq!(step(&mut est90, &mut t90, 11_111_111), Some(11_111_111));
        for i in 1..=4 {
            let got = step(&mut est90, &mut t90, 16_666_667).unwrap();
            if i < 4 {
                assert_eq!(got, 11_111_111);
            } else {
                assert_eq!(got, 16_666_667);
            }
        }
    }
    // spec: AND-APP-10
    #[test]
    fn vsync_is_requested_only_for_pending_content_and_coalesces_wakes() {
        assert!(!request_vsync(None, false));
        let due = Instant::now() - Duration::from_millis(1);
        assert!(request_vsync(Some(due), false));
        assert!(!request_vsync(Some(due), true));
        assert!(!request_vsync(
            Some(Instant::now() + Duration::from_secs(1)),
            false
        ));
    }
    // spec: HOST-SES-3
    #[test]
    fn menu_mailbox_coalesces_and_commands_use_the_existing_waking_input_channel() {
        let wake = WakeEvent::new().unwrap();
        let keys = Arc::new(KeyState::default());
        let (mut worker, mut ui) = channel(wake.clone(), keys.clone()).unwrap();
        for revision in 1..=10 {
            worker.publish_menu(MenuBar {
                key: MenuKey {
                    window: 1,
                    menu: 2,
                    revision,
                    enabled: true,
                },
                items: Vec::new(),
                popup: None,
            });
        }
        assert!(ui.menus.update());
        assert_eq!(ui.menus.current().key.revision, 10);
        assert!(!ui.menus.update());
        assert!(!worker.needs_menu(ui.menus.current().key));
        assert!(
            !wake.wait(Some(Duration::ZERO)).unwrap(),
            "menu output does not wake the guest"
        );
        let command = adv_runtime::MenuCommand {
            window: 1,
            root: 2,
            menu: 2,
            position: 0,
            id: 77,
        };
        ui.pump_events(
            vec![window::Event::Menu(adv_runtime::MenuEvent::Command(
                command,
            ))],
            Instant::now(),
        )
        .unwrap();
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        let mut performance = performance::Performance::default();
        assert!(worker.pump(&mut performance).into_iter().any(|event|matches!(event,window::Event::Menu(adv_runtime::MenuEvent::Command(actual)) if actual==command)));
        assert_eq!(keys.get(0x11), 0, "menu clicks do not change physical Ctrl");
    }

    // spec: HOST-SES-4
    #[cfg(host_view)]
    #[test]
    fn post_text_uses_input_delivery_and_returns_the_guest_count() {
        let wake = WakeEvent::new().unwrap();
        let (mut worker, mut ui) = channel(wake.clone(), Arc::new(KeyState::default())).unwrap();
        let execution = std::thread::spawn(move || {
            assert!(wake.wait(Some(Duration::from_secs(1))).unwrap());
            let events = worker.pump(&mut performance::Performance::default());
            let window::Event::Text(text, reply) = &events[0] else {
                panic!("expected text input")
            };
            assert_eq!(text, "あいう");
            reply.send(2).unwrap();
        });
        assert_eq!(ui.post_text("あいう"), 2);
        execution.join().unwrap();
    }

    // spec: HOST-SES-2
    #[test]
    fn repaint_and_telemetry_do_not_wake_execution_but_real_input_does() {
        let wake = WakeEvent::new().unwrap();
        let keys = Arc::new(KeyState::default());
        let (mut worker, mut ui) = channel(wake.clone(), keys.clone()).unwrap();
        assert!(
            ui.pump_events(vec![window::Event::Repaint], Instant::now())
                .unwrap()
        );
        ui.metric(Delivery::Present(
            1,
            Duration::from_millis(1),
            Instant::now(),
        ));
        #[cfg(host_view)]
        ui.metric(Delivery::Native(performance::NativePresentation {
            consumed: 2,
            uploads: 1,
            presented: 1,
            ..Default::default()
        }));
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.frontend_pending());
        assert_eq!(
            wake.interrupt_flag().load(Ordering::Acquire) & adv_sync::pending::INTERRUPT,
            0
        );
        let mut performance = performance::Performance::default();
        assert!(worker.pump(&mut performance).is_empty());
        #[cfg(host_view)]
        {
            performance.reset(1);
            assert!(performance.json().contains("\"software_uploads\": 1"));
        }
        #[cfg(mobile)]
        keys.set(17, true);
        ui.pump_events(
            vec![window::Event::Input {
                message: 0x100,
                wparam: 17,
                lparam: 1,
            }],
            Instant::now(),
        )
        .unwrap();
        assert_eq!(keys.get(17) & 0x80, 0x80);
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(wake.frontend_pending());
        wake.clear_frontend();
        assert!(matches!(
            worker.pump(&mut performance).as_slice(),
            [window::Event::Input {
                message: 0x100,
                wparam: 17,
                ..
            }]
        ));
        ui.pump_events(vec![window::Event::FocusLost], Instant::now())
            .unwrap();
        assert_eq!(keys.get(17) & 0x80, 0);
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(wake.frontend_pending());
        #[cfg(host_view)]
        {
            wake.clear_frontend();
            ui.pump_events(vec![window::Event::ForceClose], Instant::now())
                .unwrap();
            assert!(worker.stopped());
            assert!(wake.wait(Some(Duration::ZERO)).unwrap());
            assert!(wake.frontend_pending());
        }
    }

    // spec: HOST-SES-3
    #[test]
    fn saturated_telemetry_keeps_reserved_input_capacity_and_reports_drops() {
        let wake = WakeEvent::new().unwrap();
        let (mut worker, mut ui) = channel(wake.clone(), Arc::new(KeyState::default())).unwrap();
        for n in 0..METRIC_LIMIT {
            ui.metric(Delivery::Present(
                n as u64 + 1,
                Duration::ZERO,
                Instant::now(),
            ));
        }
        assert_eq!(ui.events.borrow().available(), RESERVED_INPUT);
        for _ in 0..1024 {
            ui.metric(Delivery::Pump(Duration::ZERO, Instant::now()));
        }
        assert_eq!(ui.metrics_dropped.load(Ordering::Relaxed), 1024);
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        for n in 0..RESERVED_INPUT {
            ui.deliver(Delivery::Input(window::Event::Input {
                message: 0x101,
                wparam: n as u32,
                lparam: 0,
            }))
            .unwrap();
        }
        assert!(ui.events.borrow().is_full());
        ui.metric(Delivery::Present(9999, Duration::ZERO, Instant::now()));
        assert!(!worker.stopped());
        let mut performance = performance::Performance::default();
        let events = worker.pump(&mut performance);
        assert_eq!(events.len(), RESERVED_INPUT);
        assert!(
            matches!(events.last(),Some(window::Event::Input{message:0x101,wparam,..}) if *wparam==RESERVED_INPUT as u32-1)
        );
        assert!(
            performance
                .json()
                .contains("\"ui_metrics_dropped_total\": 1025")
        );
        assert_eq!(ui.events.borrow().available(), INPUT_CAPACITY);
        ui.metric(Delivery::Present(10000, Duration::ZERO, Instant::now()));
        assert_eq!(ui.events.borrow().available(), INPUT_CAPACITY - 1);
    }
    // spec: PERS-SCOPE-10
    #[test]
    fn modifier_messages_press_the_generic_and_side_keys() {
        let keys = KeyState::default();
        update_keys(&keys, 0x100, 0x11, 0x001d_0001);
        update_keys(&keys, 0x104, 0x12, 0x0138_0001);
        update_keys(&keys, 0x100, 0x10, 0x0036_0001);
        for key in [0x10, 0x11, 0x12, 0xa1, 0xa2, 0xa5] {
            assert_eq!(keys.get(key) & 0x80, 0x80, "{key:#x}");
        }
        for key in [0xa0, 0xa3, 0xa4] {
            assert_eq!(keys.get(key), 0, "{key:#x}");
        }
        update_keys(&keys, 0x101, 0x11, 0xc01d_0001);
        assert_eq!(keys.get(0x11) | keys.get(0xa2), 0);
        // Mouse button state flags never alias modifier keys.
        update_keys(&keys, 0x201, 0x10, 0);
        assert_eq!(keys.get(0xa0), 0);
    }
}
