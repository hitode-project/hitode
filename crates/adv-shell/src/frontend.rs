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
    #[cfg(any(target_os = "macos", target_os = "ios"))]
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
            #[cfg(any(target_os = "macos", target_os = "ios"))]
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
        self.execution_wake.signal();
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
                window::Event::FocusLost | window::Event::Close => self.keys.release_all(),
                window::Event::Repaint => {
                    repaint = true;
                    continue;
                }
                #[cfg(host_view)]
                window::Event::AudioSuspended(_)
                | window::Event::Volume(_)
                | window::Event::Paused(_)
                | window::Event::Text(..) => {}
                #[cfg(host_view)]
                window::Event::ForceClose => {
                    self.stopped.store(true, Ordering::Release);
                    self.execution_wake.signal();
                    continue;
                }
                window::Event::Menu(_) | window::Event::Dialog(_) => {}
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
        let started = Instant::now();
        let events = window.take_events();
        self.repaint |= self.pump_events(events, started)?;

        if self.native.is_some() {
            let dimensions = window.native_client_size()?;
            #[cfg(target_vendor = "apple")]
            let scale = window.native_scale();
            #[cfg(target_os = "android")]
            let scale = 1.;
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
            execution_wake.signal();
            Ok(())
        }));
        let events = self.events.clone();
        let execution_wake = self.execution_wake.clone();
        window.set_dialog_event_sink(Box::new(move |event| {
            events
                .borrow_mut()
                .push(Delivery::Input(window::Event::Dialog(event)))
                .map_err(|_| "UI dialog input channel capacity exceeded".to_owned())?;
            execution_wake.signal();
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
/// Whether a vsync `elapsed_ns` after the last counted one is a presentation
/// refresh: faster panels count every n-th vsync, the fewest that keep
/// presentation at or below 60 Hz.
#[cfg(any(target_os = "android", test))]
pub(crate) fn counts_refresh(elapsed_ns: i64, period_ns: i64) -> bool {
    const SIXTY: i64 = 16_666_667;
    let period = if period_ns > 0 { period_ns } else { SIXTY };
    // The tolerance absorbs periods a little shorter than an exact divisor.
    let every = ((SIXTY as f64 / period as f64 - 0.05).ceil() as i64).max(1);
    elapsed_ns >= every * period - period / 2
}
#[cfg(any(mobile, test))]
pub(crate) fn request_vsync(next: Option<Instant>, pending: bool) -> bool {
    !pending && next.is_some_and(|next| next <= Instant::now())
}
impl Drop for Ui {
    fn drop(&mut self) {
        self.keys.release_all();
        self.stopped.store(true, Ordering::Release);
        self.execution_wake.signal();
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
        let count = |period: i64, ticks: i64| {
            let mut last = i64::MIN / 2;
            (1..=ticks)
                .filter(|&n| {
                    let at = n * period + if n % 2 == 0 { 300_000 } else { -300_000 };
                    let counted = counts_refresh(at - last, period);
                    if counted {
                        last = at;
                    }
                    counted
                })
                .count()
        };
        // One second of vsyncs with jitter, at 60, 120, 90 and 144 Hz.
        assert_eq!(count(16_666_667, 60), 60);
        assert_eq!(count(8_333_333, 120), 60);
        assert_eq!(count(11_111_111, 90), 45);
        assert_eq!(count(6_944_444, 144), 48);
        assert!(counts_refresh(16_666_667, 0));
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
