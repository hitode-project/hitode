//! UI owns the native window. The execution worker owns the entire guest machine.
//! Completed raw frames and input events cross bounded ownership channels.
use crate::{performance, window};
#[cfg(not(target_os = "macos"))]
use adv_runtime::Snapshot;
use adv_runtime::{DialogSet, FrameConsumer, FrameProducer, MenuBar, MenuKey};
use adv_sync::{WakeEvent, input::KeyState, spsc};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};

enum Request {
    Hide,
    Input(u32, u32, u32),
}
enum Delivery {
    Input(window::Event),
    Present(u64, Duration, Instant),
    Pump(Duration, Instant),
    #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
    Native(performance::NativePresentation),
}
const INPUT_CAPACITY: usize = 4096;
const METRIC_LIMIT: usize = 128;
const RESERVED_INPUT: usize = INPUT_CAPACITY - METRIC_LIMIT;
pub struct Worker {
    #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
    pub(crate) gpu: Option<adv_device::gpu::Gpu>,
    incoming: spsc::Consumer<Delivery>,
    requests: spsc::Producer<Request>,
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
    events: Rc<RefCell<spsc::Producer<Delivery>>>,
    requests: spsc::Consumer<Request>,
    frames: Option<FrameConsumer>,
    menus: adv_sync::mailbox::Consumer<MenuBar>,
    dialogs: adv_sync::mailbox::Consumer<DialogSet>,
    #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
    native: Option<crate::native_gpu::UiPresenter>,
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
    let (frames_tx, frames_rx) = adv_runtime::frame_mailbox();
    let (menus_tx, menus_rx) =
        adv_sync::mailbox::channel(std::array::from_fn(|_| MenuBar::default()));
    let (dialogs_tx, dialogs_rx) =
        adv_sync::mailbox::channel(std::array::from_fn(|_| DialogSet::default()));
    let stopped = Arc::new(AtomicBool::new(false));
    let metrics_dropped = Arc::new(AtomicU64::new(0));
    Ok((
        Worker {
            #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
            gpu: None,
            incoming,
            requests: requests_tx,
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
            events: Rc::new(RefCell::new(events)),
            requests: requests_rx,
            frames: Some(frames_rx),
            menus: menus_rx,
            dialogs: dialogs_rx,
            #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
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
    pub fn take_frames(&mut self) -> FrameProducer {
        self.frames.take().expect("frame producer installed once")
    }
    pub fn pump(&mut self, performance: &mut performance::Performance) -> Vec<window::Event> {
        let mut events = Vec::new();
        while let Some(delivery) = self.incoming.pop() {
            match delivery {
                Delivery::Input(event) => events.push(event),
                Delivery::Present(serial, elapsed, at) => performance.present(serial, elapsed, at),
                Delivery::Pump(elapsed, at) => performance.pump(elapsed, at),
                #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
                Delivery::Native(stats) => performance.native(stats),
            }
        }
        performance.set_ui_metrics_dropped(self.metrics_dropped.load(Ordering::Relaxed));
        events
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn hide(&mut self) {
        let _ = self.requests.push(Request::Hide);
        self.ui_wake.signal();
    }
    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        self.requests
            .push(Request::Input(message, wparam, lparam))
            .map_err(|_| "UI request queue is full")?;
        self.ui_wake.signal();
        Ok(())
    }
}
impl Ui {
    #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
    pub(crate) fn native(
        &mut self,
        presenter: crate::native_gpu::WindowPresenter,
        sender: adv_sync::background::Sender,
    ) {
        self.native = Some(crate::native_gpu::UiPresenter::new(
            presenter,
            self.frames
                .take()
                .expect("native compositor installed once"),
            sender,
            self.wake.clone(),
        ));
    }
    fn deliver(&mut self, event: Delivery) -> Result<(), String> {
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
                    message, wparam, ..
                } => update_keys(&self.keys, *message, *wparam),
                window::Event::FocusLost | window::Event::Close => self.keys.release_all(),
                window::Event::Repaint => {
                    repaint = true;
                    continue;
                }
                #[cfg(target_os = "macos")]
                window::Event::AudioSuspended(_) => {}
                #[cfg(target_os = "macos")]
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
    pub fn run<T>(
        &mut self,
        window: &mut window::Window,
        done: &mut spsc::Consumer<T>,
    ) -> Result<T, String> {
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
        let mut next = window.next_refresh();
        #[cfg(not(target_os = "macos"))]
        let mut snapshot = Snapshot::default();
        #[cfg(not(target_os = "macos"))]
        let mut candidate = Snapshot::default();
        let mut repaint = false;
        loop {
            if let Some(result) = done.pop() {
                return Ok(result);
            }
            while let Some(request) = self.requests.pop() {
                match request {
                    Request::Hide => window.hide(),
                    Request::Input(m, w, l) => window.post_input(m, w, l)?,
                }
            }
            if self.menus.update() {
                window.set_menu_bar(self.menus.current())?;
            }
            let had_dialogs = !self.dialogs.current().dialogs.is_empty();
            if self.dialogs.update() {
                if !had_dialogs && !self.dialogs.current().dialogs.is_empty() {
                    // Physical focus moves to the native controls; their key
                    // releases no longer pass through the canvas procedure.
                    self.keys.release_all();
                }
                window.set_dialogs(self.dialogs.current())?;
            }
            let started = Instant::now();
            let events = window.pump();
            repaint |= self.pump_events(events, started)?;
            #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
            if let Some(native) = &mut self.native {
                let completed = native.poll()?;
                if let Some(completed) = completed {
                    self.metric(Delivery::Native(completed.stats));
                    repaint |= window.native_canvas(completed.rendered.canvas)?;
                    window.set_video_cursor_regions(completed.rendered.cursor_regions);
                    if matches!(
                        completed.rendered.result,
                        crate::native_gpu::DrawResult::Presented
                    ) {
                        self.metric(Delivery::Present(
                            completed.rendered.serial,
                            completed.elapsed,
                            completed.at,
                        ));
                    }
                }
                let native = self.native.as_mut().unwrap();
                let dimensions = window.native_client_size()?;
                if Instant::now() >= next
                    && (native.has_update() || repaint)
                    && native.schedule(dimensions, repaint)?
                {
                    repaint = false;
                    next = window.next_refresh();
                }
                let delay = (!native.in_flight()
                    && (native.has_update() || repaint)
                    && dimensions.0 != 0
                    && dimensions.1 != 0)
                    .then(|| next.saturating_duration_since(Instant::now()));
                window.wait_with_wake(delay, Some(&self.wake))?;
                continue;
            }
            #[cfg(not(target_os = "macos"))]
            {
                let frames = self.frames.as_mut().expect("software UI owns its consumer");
                if Instant::now() >= next && (frames.has_update() || repaint) {
                    let present_started = Instant::now();
                    let updated = frames.update();
                    let cursor_regions = updated
                        .then(|| frames.current().video_cursor_regions())
                        .transpose()?;
                    let changed = if updated {
                        frames.current().snapshot(&mut candidate)?;
                        let changed = snapshot.width != candidate.width
                            || snapshot.height != candidate.height
                            || snapshot.bgra != candidate.bgra;
                        std::mem::swap(&mut snapshot, &mut candidate);
                        changed
                    } else {
                        false
                    };
                    if (changed || repaint) && !snapshot.bgra.is_empty() {
                        window.present(&snapshot)?;
                        // Metrics travel with input, but alone never wake an idle guest.
                        self.metric(Delivery::Present(
                            snapshot.frames,
                            present_started.elapsed(),
                            Instant::now(),
                        ));
                    }
                    if let Some(regions) = cursor_regions {
                        window.set_video_cursor_regions(Arc::from(regions));
                    }
                    repaint = false;
                    next = window.next_refresh();
                }
                let delay = (self.frames.as_ref().unwrap().has_update() || repaint)
                    .then(|| next.saturating_duration_since(Instant::now()));
                window.wait_with_wake(delay, Some(&self.wake))?;
            }
        }
    }
    pub fn wake(&self) -> WakeEvent {
        self.wake.clone()
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        self.keys.release_all();
        self.stopped.store(true, Ordering::Release);
        self.execution_wake.signal();
    }
}
pub fn update_keys(keys: &KeyState, message: u32, key: u32) {
    match message {
        0x100 | 0x104 => keys.set(key, true),
        0x101 | 0x105 => keys.set(key, false),
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
        #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
        ui.metric(Delivery::Native(performance::NativePresentation {
            consumed: 2,
            uploads: 1,
            presented: 1,
            ..Default::default()
        }));
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.interrupt_flag().load(Ordering::Acquire));
        let mut performance = performance::Performance::default();
        assert!(worker.pump(&mut performance).is_empty());
        #[cfg(any(target_os = "macos", all(windows, feature = "gpu")))]
        {
            performance.reset(1);
            assert!(performance.json().contains("\"software_uploads\": 1"));
        }
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
}
