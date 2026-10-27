//! Explicit diagnostic control. The reader never touches guest or window state.
use crate::window::WakeEvent;
use adv_sync::spsc;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Click(i16, i16),
    MouseDown(i16, i16),
    MouseUp(i16, i16),
    RightMouseDown(i16, i16),
    RightMouseUp(i16, i16),
    Move(i16, i16),
    Key(u32),
    KeyDown(u32),
    KeyUp(u32),
    Text(String),
    Activate(bool),
    Windows,
    Hit(i16, i16),
    Menus,
    Menu(u32),
    Dialogs,
    Dialog(u32),
    DialogTrack(u32, i32),
    DialogClose,
    Capture(PathBuf),
    Report(PathBuf),
    Memory(u32, u32),
    ResetMetrics,
    AwaitFrame(FrameGateSpec),
    Resume,
    Close,
    Error(String),
}

impl Command {
    fn interrupts_guest(&self) -> bool {
        matches!(
            self,
            Self::Click(..)
                | Self::MouseDown(..)
                | Self::MouseUp(..)
                | Self::RightMouseDown(..)
                | Self::RightMouseUp(..)
                | Self::Move(..)
                | Self::Key(_)
                | Self::KeyDown(_)
                | Self::KeyUp(_)
                | Self::Text(_)
                | Self::Activate(_)
                | Self::Menu(_)
                | Self::Dialog(_)
                | Self::DialogTrack(..)
                | Self::DialogClose
                | Self::Close
        )
    }
    fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match command {
            "menus" if rest.is_empty() => Ok(Self::Menus),
            "dialogs" if rest.is_empty() => Ok(Self::Dialogs),
            "dialog-close" if rest.is_empty() => Ok(Self::DialogClose),
            "dialog" => Ok(Self::Dialog(rest.parse().map_err(|_| "dialog requires a decimal control ID")?)),
            "dialog-track" => {
                let mut fields = rest.split_whitespace();
                let id = fields.next().ok_or("dialog-track requires ID VALUE")?
                    .parse().map_err(|_| "invalid control ID")?;
                let value = fields.next().ok_or("dialog-track requires ID VALUE")?
                    .parse().map_err(|_| "invalid trackbar value")?;
                if fields.next().is_some() { return Err("dialog-track requires ID VALUE".into()); }
                Ok(Self::DialogTrack(id, value))
            }
            "menu" => Ok(Self::Menu(rest.parse().map_err(|_| "menu requires a decimal command ID")?)),
            "windows" if rest.is_empty() => Ok(Self::Windows),
            "click" | "move" | "mouse-down" | "mouse-up" | "right-mouse-down" | "right-mouse-up"
            | "hit" => {
                let values: Vec<_> = rest.split_whitespace().collect();
                if values.len() != 2 {
                    return Err(format!("{command} requires two signed client coordinates"));
                }
                let x = values[0].parse().map_err(|_| "invalid mouse X")?;
                let y = values[1].parse().map_err(|_| "invalid mouse Y")?;
                Ok(match command {
                    "click" => Self::Click(x, y),
                    "mouse-down" => Self::MouseDown(x, y),
                    "mouse-up" => Self::MouseUp(x, y),
                    "right-mouse-down" => Self::RightMouseDown(x, y),
                    "right-mouse-up" => Self::RightMouseUp(x, y),
                    "hit" => Self::Hit(x, y),
                    _ => Self::Move(x, y),
                })
            }
            "key" | "key-down" | "key-up" => {
                let key = if let Some(hex) = rest.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16)
                } else {
                    rest.parse()
                }
                .map_err(|_| "key requires a decimal or 0x-prefixed virtual key")?;
                if key > 255 {
                    return Err("virtual key must be in 0..=255".into());
                }
                Ok(match command {
                    "key-down" => Self::KeyDown(key),
                    "key-up" => Self::KeyUp(key),
                    _ => Self::Key(key),
                })
            }
            "text" if !rest.is_empty() => Ok(Self::Text(rest.to_owned())),
            "activate" => match rest {
                "0" => Ok(Self::Activate(false)),
                "1" => Ok(Self::Activate(true)),
                _ => Err("activate requires 0 or 1".into()),
            },
            "memory" => {
                let mut fields = rest.split_whitespace();
                let address = fields.next().ok_or("memory requires ADDRESS LENGTH")?;
                let address = if let Some(hex) = address.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16)
                } else { address.parse() }.map_err(|_| "invalid guest address")?;
                let len: u32 = fields.next().ok_or("memory requires LENGTH")?
                    .parse().map_err(|_| "invalid memory length")?;
                if !(1..=64).contains(&len) || fields.next().is_some()
                    || u64::from(address) + u64::from(len) > 1u64 << 32 {
                    return Err("memory range must be 1..=64 bytes inside guest address space".into());
                }
                Ok(Self::Memory(address, len))
            }
            "capture" | "report" => {
                if rest.is_empty() {
                    return Err(format!("{command} requires a path"));
                }
                let path = rest
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(rest)
                    .into();
                Ok(if command == "capture" {
                    Self::Capture(path)
                } else {
                    Self::Report(path)
                })
            }
            "close" if rest.is_empty() => Ok(Self::Close),
            "resume" if rest.is_empty() => Ok(Self::Resume),
            "await-frame" => Ok(Self::AwaitFrame(FrameGateSpec::parse(rest)?)),
            "reset-metrics" if rest.is_empty() => Ok(Self::ResetMetrics),
            _ => Err(
                "expected click/move X Y, key/key-down/key-up VK, text STRING, activate 0|1, windows, hit X Y, menus, menu ID, dialogs, dialog ID, dialog-track ID VALUE, dialog-close, capture PATH, report PATH, memory ADDRESS LENGTH, reset-metrics, await-frame HASH TIMEOUT_MS [hold VK] [release VK] [current] [probes X,Y,B,G,R,A;...], resume or close"
                    .into(),
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PixelProbe {
    pub x: u32,
    pub y: u32,
    pub bgra: [u8; 4],
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameGateSpec {
    pub hash: u64,
    pub timeout: Duration,
    pub hold: Option<u32>,
    pub release: Option<u32>,
    pub current: bool,
    pub probes: Vec<PixelProbe>,
}
impl FrameGateSpec {
    fn parse(rest: &str) -> Result<Self, String> {
        let mut words = rest.split_whitespace();
        let hash = words.next().ok_or("await-frame requires HASH TIMEOUT_MS")?;
        if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("await-frame hash must have exactly 16 hexadecimal digits".into());
        }
        let hash = u64::from_str_radix(hash, 16).map_err(|_| "invalid frame hash")?;
        let timeout: u64 = words
            .next()
            .ok_or("await-frame requires TIMEOUT_MS")?
            .parse()
            .map_err(|_| "invalid frame timeout")?;
        if !(1..=600_000).contains(&timeout) {
            return Err("frame timeout must be in 1..=600000 milliseconds".into());
        }
        let mut spec = Self {
            hash,
            timeout: Duration::from_millis(timeout),
            hold: None,
            release: None,
            current: false,
            probes: Vec::new(),
        };
        while let Some(option) = words.next() {
            match option {
                "hold" | "release" => {
                    let key: u32 = words
                        .next()
                        .ok_or("hold/release requires a decimal key")?
                        .parse()
                        .map_err(|_| "invalid hold/release key")?;
                    if key > 255 {
                        return Err("virtual key must be in 0..=255".into());
                    }
                    let target = if option == "hold" {
                        &mut spec.hold
                    } else {
                        &mut spec.release
                    };
                    if target.replace(key).is_some() {
                        return Err("duplicate hold/release option".into());
                    }
                }
                "current" if !spec.current => spec.current = true,
                "probes" if spec.probes.is_empty() => {
                    let probes = words.next().ok_or("probes requires X,Y,B,G,R,A;...")?;
                    for probe in probes.split(';') {
                        let values: Vec<u32> = probe
                            .split(',')
                            .map(str::parse)
                            .collect::<Result<_, _>>()
                            .map_err(|_| "invalid pixel probe")?;
                        if values.len() != 6
                            || values[0] >= 8192
                            || values[1] >= 8192
                            || values[2..].iter().any(|&v| v > 255)
                            || spec.probes.len() >= 16
                        {
                            return Err("probes requires 1..=16 bounded X,Y,B,G,R,A pixels".into());
                        }
                        spec.probes.push(PixelProbe {
                            x: values[0],
                            y: values[1],
                            bgra: [
                                values[2] as u8,
                                values[3] as u8,
                                values[4] as u8,
                                values[5] as u8,
                            ],
                        });
                    }
                }
                _ => return Err("invalid or duplicate await-frame option".into()),
            }
        }
        if spec.hold.is_some() && spec.current {
            return Err(
                "current cannot be combined with hold: an input must reach a new frame".into(),
            );
        }
        if let Some(hold) = spec.hold {
            if spec.release.is_some_and(|release| release != hold) {
                return Err("held key must be released by the same gate".into());
            }
            spec.release = Some(hold);
        }
        Ok(spec)
    }
}

struct ArmedGate {
    spec: FrameGateSpec,
    start_frame: u64,
    observed_frame: Option<u64>,
    deadline: Instant,
}
pub struct GateCompletion {
    pub release: Option<u32>,
    pub result: Result<String, String>,
}
/// Opt-in frame diagnostics. No guest work runs between a latch and its evidence.
#[derive(Default)]
pub struct SceneGate {
    pending: Option<ArmedGate>,
    frozen_since: Option<Instant>,
    frozen_wall: Duration,
    pub failure: Option<String>,
    arms: u64,
    matches: u64,
    checks: u64,
    hashes: u64,
    check_time: Duration,
    hash_time: Duration,
    hash_max: Duration,
    frame_captures: u64,
    frame_capture_time: Duration,
}
impl SceneGate {
    pub fn arm(&mut self, spec: FrameGateSpec, frame: u64, now: Instant) -> Result<(), String> {
        if self.pending.is_some() || self.failure.is_some() {
            return Err("cannot arm a pending or failed scene gate".into());
        }
        self.resume(now)?;
        self.arms += 1;
        self.pending = Some(ArmedGate {
            deadline: now + spec.timeout,
            spec,
            start_frame: frame,
            observed_frame: None,
        });
        Ok(())
    }
    pub fn resume(&mut self, now: Instant) -> Result<(), String> {
        if self.failure.is_some() {
            return Err("failed scene gate remains frozen".into());
        }
        if let Some(since) = self.frozen_since.take() {
            self.frozen_wall += now.saturating_duration_since(since);
        }
        Ok(())
    }
    pub fn close(&mut self, now: Instant) {
        if let Some(since) = self.frozen_since.take() {
            self.frozen_wall += now.saturating_duration_since(since);
        }
    }
    pub fn frozen(&self) -> bool {
        self.frozen_since.is_some()
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn needs_frame_pixels(&self, presented: u64, captured: u64) -> bool {
        self.pending() && presented != 0 && presented != captured
    }
    pub fn frame_capture(&mut self, elapsed: Duration) {
        self.frame_captures += 1;
        self.frame_capture_time += elapsed;
        self.check_time += elapsed;
    }
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.pending
            .as_ref()
            .map(|gate| gate.deadline.saturating_duration_since(now))
    }
    pub fn abort(&mut self, reason: &str, frame: u64, now: Instant) -> Option<GateCompletion> {
        let gate = self.pending.take()?;
        let error = format!(
            "await-frame {reason} target={:016x} frame={frame} start_frame={} frozen=true",
            gate.spec.hash, gate.start_frame
        );
        self.failure = Some(error.clone());
        self.frozen_since = Some(now);
        Some(GateCompletion {
            release: gate.spec.release,
            result: Err(error),
        })
    }
    pub fn observe(
        &mut self,
        frame: u64,
        width: u32,
        pixels: &[u8],
        now: Instant,
    ) -> Option<GateCompletion> {
        let gate = self.pending.as_mut()?;
        if now >= gate.deadline {
            return self.abort("timeout", frame, now);
        }
        if frame == 0
            || gate.observed_frame == Some(frame)
            || (!gate.spec.current && frame <= gate.start_frame)
        {
            return None;
        }
        gate.observed_frame = Some(frame);
        let started = Instant::now();
        self.checks += 1;
        let probes_match = gate.spec.probes.iter().all(|p| {
            if p.x >= width {
                return false;
            }
            let offset = ((u64::from(p.y) * u64::from(width) + u64::from(p.x)) * 4) as usize;
            pixels.get(offset..offset.saturating_add(4)) == Some(p.bgra.as_slice())
        });
        let matched = if probes_match {
            let hash_started = Instant::now();
            let hash = pixels.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
            let elapsed = hash_started.elapsed();
            self.hashes += 1;
            self.hash_time += elapsed;
            self.hash_max = self.hash_max.max(elapsed);
            hash == gate.spec.hash
        } else {
            false
        };
        self.check_time += started.elapsed();
        if !matched {
            return None;
        }
        let gate = self.pending.take().unwrap();
        self.matches += 1;
        self.frozen_since = Some(Instant::now());
        Some(GateCompletion {
            release: gate.spec.release,
            result: Ok(format!(
                "await-frame hash={:016x} frame={frame} start_frame={} frozen=true",
                gate.spec.hash, gate.start_frame
            )),
        })
    }
    pub fn check_duration(&self) -> Duration {
        self.check_time
    }
    pub fn json(&self) -> String {
        let frozen = self.frozen_wall
            + self
                .frozen_since
                .map_or(Duration::ZERO, |since| since.elapsed());
        format!(
            concat!(
                "{{\"arms\":{},\"matches\":{},\"pending\":{},\"frozen\":{},",
                "\"frame_checks\":{},\"full_hash_checks\":{},\"check_seconds\":{:.9},",
                "\"hash_seconds\":{:.9},\"hash_max_seconds\":{:.9},\"frozen_observation_seconds\":{:.9},",
                "\"frame_captures\":{},\"frame_capture_seconds\":{:.9}}}"
            ),
            self.arms,
            self.matches,
            self.pending(),
            self.frozen(),
            self.checks,
            self.hashes,
            self.check_time.as_secs_f64(),
            self.hash_time.as_secs_f64(),
            self.hash_max.as_secs_f64(),
            frozen.as_secs_f64(),
            self.frame_captures,
            self.frame_capture_time.as_secs_f64()
        )
    }
}

pub fn acknowledge(result: Result<String, String>) {
    match result {
        Ok(message) => println!("control ok {message}"),
        Err(error) => println!("control error {error}"),
    }
    let _ = std::io::stdout().flush();
}

const CONTROL_CAPACITY: usize = 128;
struct Packet {
    sequence: u64,
    command: Command,
}
struct Sender {
    input: spsc::Producer<Packet>,
    diagnostics: spsc::Producer<Packet>,
    sequence: u64,
}
struct Inbox {
    input: spsc::Consumer<Packet>,
    diagnostics: spsc::Consumer<Packet>,
    input_head: Option<Packet>,
    diagnostic_head: Option<Packet>,
}
fn channel() -> (Sender, Inbox) {
    let (input, input_rx) = spsc::channel(CONTROL_CAPACITY);
    let (diagnostics, diagnostics_rx) = spsc::channel(CONTROL_CAPACITY);
    (
        Sender {
            input,
            diagnostics,
            sequence: 0,
        },
        Inbox {
            input: input_rx,
            diagnostics: diagnostics_rx,
            input_head: None,
            diagnostic_head: None,
        },
    )
}
impl Sender {
    fn send(&mut self, command: Command) -> Result<(), Command> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("control sequence exhausted");
        let queue = if command.interrupts_guest() {
            &mut self.input
        } else {
            &mut self.diagnostics
        };
        queue
            .push(Packet {
                sequence: self.sequence,
                command,
            })
            .map_err(|packet| packet.command)
    }
}
impl Inbox {
    fn fill(&mut self) {
        if self.input_head.is_none() {
            self.input_head = self.input.pop();
        }
        if self.diagnostic_head.is_none() {
            self.diagnostic_head = self.diagnostics.pop();
        }
    }
    fn pop_input(&mut self) -> Option<Command> {
        self.input_head.take().map(|packet| packet.command)
    }
    fn pop_first(&mut self) -> Option<Command> {
        let input_first = self.input_head.as_ref().is_some_and(|input| {
            self.diagnostic_head
                .as_ref()
                .is_none_or(|diagnostic| input.sequence < diagnostic.sequence)
        });
        if input_first {
            self.pop_input()
        } else {
            self.diagnostic_head.take().map(|packet| packet.command)
        }
    }
    fn is_empty(&self) -> bool {
        self.input_head.is_none()
            && self.diagnostic_head.is_none()
            && self.input.is_empty()
            && self.diagnostics.is_empty()
    }
}

pub struct Control {
    receiver: Inbox,
    reader: Option<std::thread::Thread>,
    alive: Arc<AtomicBool>,
    awaiting_guest_boundary: bool,
    awaiting_until: Option<Instant>,
}
impl Control {
    pub fn start(
        wake: WakeEvent,
        keys: std::sync::Arc<adv_sync::input::KeyState>,
    ) -> Result<Self, String> {
        let (mut sender, receiver) = channel();
        let reader_wake = wake.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let reader_alive = alive.clone();
        let reader = std::thread::Builder::new()
            .name("adv-control-stdin".into())
            .spawn(move || {
                for line in std::io::stdin().lock().lines() {
                    let failed = line.is_err();
                    let command = match line {
                        Ok(line) if line.trim().is_empty() => continue,
                        Ok(line) => Command::parse(&line).unwrap_or_else(Command::Error),
                        Err(error) => Command::Error(format!("stdin read failed: {error}")),
                    };
                    match &command {
                        Command::MouseDown(..) => keys.set(1, true),
                        Command::MouseUp(..) => keys.set(1, false),
                        Command::RightMouseDown(..) => keys.set(2, true),
                        Command::RightMouseUp(..) => keys.set(2, false),
                        Command::KeyDown(key) => {
                            crate::frontend::update_keys(&keys, 0x100, *key, 1)
                        }
                        Command::KeyUp(key) => {
                            crate::frontend::update_keys(&keys, 0x101, *key, 0xc000_0001)
                        }
                        Command::Close => keys.release_all(),
                        _ => {}
                    }
                    let interrupt = command.interrupts_guest();
                    let mut pending = command;
                    loop {
                        if !reader_alive.load(Ordering::Acquire) {
                            return;
                        }
                        match sender.send(pending) {
                            Ok(()) => break,
                            Err(command) if !interrupt => {
                                acknowledge(Err(format!("diagnostic queue full: {command:?}")));
                                break;
                            }
                            Err(command) => {
                                pending = command;
                                reader_wake.signal();
                                // Only this producer parks for backpressure.
                                // The guest worker never waits for the reader;
                                // unpark retains a token across this race.
                                std::thread::park();
                            }
                        }
                    }
                    if interrupt {
                        reader_wake.signal();
                    } else {
                        reader_wake.notify();
                    }
                    if failed {
                        break;
                    }
                }
                drop(sender);
                reader_wake.notify();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            receiver,
            reader: Some(reader.thread().clone()),
            alive,
            awaiting_guest_boundary: false,
            awaiting_until: None,
        })
    }
    pub fn drain(&mut self) -> Vec<Command> {
        self.drain_frozen(false)
    }
    pub fn drain_frozen(&mut self, frozen: bool) -> Vec<Command> {
        self.receiver.fill();
        if let Some(reader) = &self.reader {
            reader.unpark();
        }
        // An interrupt can produce a slice with no guest progress, or stop a
        // partial message dispatch. Keep diagnostics behind a normal guest
        // boundary after input, while new input and close remain responsive.
        let command = if self.awaiting_guest_boundary && !frozen {
            let input = self.receiver.pop_input();
            if input.is_some() {
                input
            } else if self
                .awaiting_until
                .is_some_and(|until| Instant::now() >= until)
            {
                // Never silently claim an ignored input was handled. Bound the
                // diagnostic wait with a visible error; actual close still wins.
                self.awaiting_guest_boundary = false;
                self.awaiting_until = None;
                self.receiver.pop_first().map(|command| {
                    Command::Error(format!(
                        "input observation fence timed out before {command:?}"
                    ))
                })
            } else {
                None
            }
        } else {
            self.receiver.pop_first()
        };
        if command.as_ref().is_some_and(Command::interrupts_guest) {
            self.awaiting_guest_boundary = true;
            self.awaiting_until = Some(Instant::now() + Duration::from_secs(1));
        }
        command.into_iter().collect()
    }
    /// `ordinary_boundary` is true when the guest completed a normal slice or
    /// blocked without an external-interrupt exit. Partial IRQ progress alone
    /// does not establish that the queued input reached its guest procedure.
    pub fn after_guest_step(&mut self, ordinary_boundary: bool) {
        if ordinary_boundary {
            self.awaiting_guest_boundary = false;
            self.awaiting_until = None;
        }
    }
    pub fn has_pending(&self) -> bool {
        !self.receiver.is_empty()
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Some(reader) = &self.reader {
            reader.unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_reads_only_pending_uncaptured_frames_and_accounts_observer_cost() {
        let mut gate = SceneGate::default();
        assert!(!gate.needs_frame_pixels(8, 7));
        gate.arm(
            FrameGateSpec::parse("1111111111111111 1000").unwrap(),
            7,
            Instant::now(),
        )
        .unwrap();
        assert!(gate.needs_frame_pixels(8, 7));
        assert!(!gate.needs_frame_pixels(8, 8));
        assert!(!gate.needs_frame_pixels(0, 7));
        gate.frame_capture(Duration::from_millis(3));
        assert_eq!(gate.check_duration(), Duration::from_millis(3));
        assert!(
            gate.json()
                .contains("\"frame_capture_seconds\":0.003000000")
        );
        gate.abort("fixture complete", 8, Instant::now());
        assert!(!gate.needs_frame_pixels(9, 8));
    }
    #[test]
    fn saturated_diagnostics_cannot_consume_input_capacity_or_grow_pending_storage() {
        let (mut sender, receiver) = channel();
        for _ in 0..CONTROL_CAPACITY {
            sender.send(Command::ResetMetrics).unwrap();
        }
        assert_eq!(
            sender.send(Command::ResetMetrics),
            Err(Command::ResetMetrics)
        );
        sender.send(Command::KeyUp(17)).unwrap();
        sender.send(Command::Close).unwrap();
        let mut control = Control {
            receiver,
            reader: None,
            alive: Arc::new(AtomicBool::new(true)),
            awaiting_guest_boundary: true,
            awaiting_until: Some(Instant::now() + Duration::from_secs(1)),
        };
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        assert_eq!(control.drain(), [Command::Close]);
        control.after_guest_step(true);
        for _ in 0..CONTROL_CAPACITY {
            assert_eq!(control.drain(), [Command::ResetMetrics]);
        }
        assert!(!control.has_pending());
        // Input saturation returns ownership to the reader for parked retry.
        for _ in 0..CONTROL_CAPACITY {
            sender.send(Command::KeyDown(65)).unwrap();
        }
        assert_eq!(sender.send(Command::KeyUp(65)), Err(Command::KeyUp(65)));
    }
    #[test]
    fn commands_preserve_paths_and_reject_invalid_packets() {
        assert_eq!(
            Command::parse("click 650 410"),
            Ok(Command::Click(650, 410))
        );
        assert_eq!(Command::parse("key 0x0d"), Ok(Command::Key(13)));
        assert_eq!(Command::parse("windows"), Ok(Command::Windows));
        assert_eq!(Command::parse("hit 640 -1"), Ok(Command::Hit(640, -1)));
        assert!(Command::parse("hit 1").is_err());
        assert!(Command::parse("windows 1").is_err());
        assert_eq!(Command::parse("move -12 410"), Ok(Command::Move(-12, 410)));
        assert_eq!(
            Command::parse("mouse-down -1 620"),
            Ok(Command::MouseDown(-1, 620))
        );
        assert_eq!(
            Command::parse("mouse-up 358 620"),
            Ok(Command::MouseUp(358, 620))
        );
        assert!(Command::parse("mouse-down 1").is_err());
        assert!(Command::parse("mouse-up 32768 0").is_err());
        assert!(Command::MouseDown(0, 0).interrupts_guest());
        assert!(Command::MouseUp(0, 0).interrupts_guest());
        assert_eq!(
            Command::parse("right-mouse-down -1 620"),
            Ok(Command::RightMouseDown(-1, 620))
        );
        assert_eq!(
            Command::parse("right-mouse-up 358 620"),
            Ok(Command::RightMouseUp(358, 620))
        );
        assert!(Command::parse("right-mouse-down 1").is_err());
        assert!(Command::parse("right-mouse-up 32768 0").is_err());
        assert!(Command::RightMouseDown(0, 0).interrupts_guest());
        assert!(Command::RightMouseUp(0, 0).interrupts_guest());
        assert_eq!(Command::parse("key-down 0x11"), Ok(Command::KeyDown(17)));
        assert_eq!(Command::parse("key-up 17"), Ok(Command::KeyUp(17)));
        assert_eq!(
            Command::parse("text  なまえ A "),
            Ok(Command::Text("なまえ A".into()))
        );
        assert!(Command::parse("text").is_err());
        assert!(Command::Text("a".into()).interrupts_guest());
        assert_eq!(Command::parse("activate 0"), Ok(Command::Activate(false)));
        assert_eq!(Command::parse("activate 1"), Ok(Command::Activate(true)));
        assert!(Command::parse("activate").is_err());
        assert_eq!(Command::parse("menus"), Ok(Command::Menus));
        assert_eq!(Command::parse("menu 100"), Ok(Command::Menu(100)));
        assert_eq!(Command::parse("dialogs"), Ok(Command::Dialogs));
        assert_eq!(Command::parse("dialog 1"), Ok(Command::Dialog(1)));
        assert_eq!(
            Command::parse("dialog-track 1002 0"),
            Ok(Command::DialogTrack(1002, 0))
        );
        assert_eq!(Command::parse("dialog-close"), Ok(Command::DialogClose));
        assert_eq!(Command::parse("reset-metrics"), Ok(Command::ResetMetrics));
        assert_eq!(Command::parse("resume"), Ok(Command::Resume));
        assert_eq!(
            Command::parse("memory 0x400000 16"),
            Ok(Command::Memory(0x400000, 16))
        );
        assert_eq!(
            Command::parse("capture target/a frame.png"),
            Ok(Command::Capture("target/a frame.png".into()))
        );
        for line in [
            "click 1",
            "click 99999 1",
            "key 256",
            "key-down 256",
            "key-up -1",
            "menu -1",
            "dialog abc",
            "dialog-close extra",
            "capture",
            "close extra",
            "reset-metrics extra",
            "memory 0xffffffff 2",
            "memory 0x400000 65",
            "memory 0x400000 0",
            "memory 0x400000 4 extra",
            "dialog-track 1002",
            "dialog-track 1002 0 extra",
            "garbage",
            "await-frame abc 100",
            "await-frame 0000000000000001 0",
            "await-frame 0000000000000001 600001",
            "await-frame 0000000000000001 100 hold 256",
            "await-frame 0000000000000001 100 hold 17 current",
            "await-frame 0000000000000001 100 hold 17 release 13",
            "await-frame 0000000000000001 100 probes 0,0,256,0,0,0",
        ] {
            assert!(Command::parse(line).is_err(), "{line}");
        }
    }
    #[test]
    fn frame_gate_requires_a_new_frame_and_freezes_before_acknowledging() {
        let pixels: [u8; 4] = [1, 2, 3, 255];
        let hash = pixels.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        let spec = FrameGateSpec::parse(&format!("{hash:016x} 100 hold 17")).unwrap();
        assert_eq!(spec.release, Some(17));
        let now = Instant::now();
        let mut gate = SceneGate::default();
        gate.arm(spec, 5, now).unwrap();
        assert!(gate.observe(5, 1, &pixels, now).is_none());
        let latch = gate.observe(6, 1, &pixels, now).unwrap();
        assert_eq!(latch.release, Some(17));
        assert!(
            latch
                .result
                .unwrap()
                .contains("frame=6 start_frame=5 frozen=true")
        );
        assert!(gate.frozen());
        assert_eq!(gate.hashes, 1);
        gate.resume(now).unwrap();
        assert!(!gate.frozen());
    }
    #[test]
    fn pixel_prefilter_cannot_accept_a_full_hash_mismatch_or_rehash_a_frame() {
        let now = Instant::now();
        let mut gate = SceneGate::default();
        let spec = FrameGateSpec::parse("0000000000000001 100 probes 0,0,1,2,3,4").unwrap();
        gate.arm(spec, 0, now).unwrap();
        assert!(gate.observe(1, 1, &[0, 0, 0, 0], now).is_none());
        assert_eq!(gate.hashes, 0);
        assert!(gate.observe(2, 1, &[1, 2, 3, 4], now).is_none());
        assert!(gate.observe(2, 1, &[1, 2, 3, 4], now).is_none());
        assert_eq!(gate.hashes, 1);
        assert!(!gate.frozen());
        let latch = gate
            .observe(2, 1, &[1, 2, 3, 4], now + Duration::from_millis(101))
            .unwrap();
        assert!(latch.result.unwrap_err().contains("timeout"));
        assert!(gate.frozen());
        assert!(gate.failure.is_some());
        assert!(gate.resume(now).is_err());
    }
    #[test]
    fn frozen_inputs_do_not_deadlock_the_next_gate_or_its_diagnostics() {
        let (mut sender, receiver) = channel();
        let mut control = Control {
            receiver,
            reader: None,
            alive: Arc::new(AtomicBool::new(true)),
            awaiting_guest_boundary: false,
            awaiting_until: None,
        };
        sender.send(Command::Key(13)).unwrap();
        sender.send(Command::Report("frozen.json".into())).unwrap();
        sender.send(Command::Resume).unwrap();
        assert_eq!(control.drain_frozen(true), [Command::Key(13)]);
        assert_eq!(
            control.drain_frozen(true),
            [Command::Report("frozen.json".into())]
        );
        assert_eq!(control.drain_frozen(true), [Command::Resume]);
        assert!(control.drain().is_empty());
        control.after_guest_step(true);
    }
    #[test]
    fn only_input_and_close_commands_request_guest_preemption() {
        for line in [
            "click 1 2",
            "move 1 2",
            "key 65",
            "key-down 17",
            "key-up 17",
            "dialog-track 1002 0",
            "close",
        ] {
            assert!(Command::parse(line).unwrap().interrupts_guest(), "{line}");
        }
        for command in [
            Command::Capture("frame.png".into()),
            Command::Report("report.json".into()),
            Command::ResetMetrics,
            Command::Error("invalid diagnostic".into()),
        ] {
            assert!(!command.interrupts_guest(), "{command:?}");
        }
    }
    #[test]
    fn diagnostic_after_input_waits_for_an_ordinary_guest_boundary() {
        let (mut sender, receiver) = channel();
        let mut control = Control {
            receiver,
            reader: None,
            alive: Arc::new(AtomicBool::new(true)),
            awaiting_guest_boundary: false,
            awaiting_until: None,
        };
        sender.send(Command::KeyUp(17)).unwrap();
        sender.send(Command::Capture("frame.png".into())).unwrap();
        sender.send(Command::Report("report.json".into())).unwrap();
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        control.after_guest_step(false);
        assert!(control.drain().is_empty());
        assert!(control.has_pending());
        // A second IRQ is also insufficient, even if its slice executed a
        // prefix. Neither notification is cleared or ignored by the controller.
        control.after_guest_step(false);
        assert!(control.drain().is_empty());
        control.after_guest_step(true);
        assert_eq!(control.drain(), [Command::Capture("frame.png".into())]);
        assert_eq!(control.drain(), [Command::Report("report.json".into())]);
        assert!(!control.has_pending());
    }
    #[test]
    fn input_and_close_overtake_diagnostics_waiting_for_guest_dispatch() {
        let (mut sender, receiver) = channel();
        let mut control = Control {
            receiver,
            reader: None,
            alive: Arc::new(AtomicBool::new(true)),
            awaiting_guest_boundary: false,
            awaiting_until: None,
        };
        sender.send(Command::KeyDown(17)).unwrap();
        sender.send(Command::Capture("frame.png".into())).unwrap();
        sender.send(Command::ResetMetrics).unwrap();
        assert_eq!(control.drain(), [Command::KeyDown(17)]);
        control.after_guest_step(false);
        sender.send(Command::KeyUp(17)).unwrap();
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        control.after_guest_step(false);
        sender.send(Command::Close).unwrap();
        assert_eq!(control.drain(), [Command::Close]);
        assert!(control.drain().is_empty());
        control.after_guest_step(true);
        assert_eq!(control.drain(), [Command::Capture("frame.png".into())]);
        assert_eq!(control.drain(), [Command::ResetMetrics]);
    }
    #[test]
    fn a_fence_without_any_observation_reports_an_error_without_waiting_forever() {
        let (mut sender, receiver) = channel();
        let mut control = Control {
            receiver,
            reader: None,
            alive: Arc::new(AtomicBool::new(true)),
            awaiting_guest_boundary: false,
            awaiting_until: None,
        };
        sender.send(Command::KeyUp(17)).unwrap();
        sender.send(Command::Capture("frame.png".into())).unwrap();
        sender.send(Command::Report("report.json".into())).unwrap();
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        control.awaiting_until = Some(Instant::now());
        assert!(
            matches!(control.drain().as_slice(), [Command::Error(error)] if error.contains("fence timed out"))
        );
        assert_eq!(control.drain(), [Command::Report("report.json".into())]);
    }
}
