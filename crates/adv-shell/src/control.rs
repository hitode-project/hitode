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
    Key(u32),
    KeyDown(u32),
    KeyUp(u32),
    Menus,
    Menu(u32),
    Dialogs,
    Dialog(u32),
    DialogClose,
    Capture(PathBuf),
    Report(PathBuf),
    ResetMetrics,
    Close,
    Error(String),
}

impl Command {
    fn interrupts_guest(&self) -> bool {
        matches!(
            self,
            Self::Click(..)
                | Self::Key(_)
                | Self::KeyDown(_)
                | Self::KeyUp(_)
                | Self::Menu(_)
                | Self::Dialog(_)
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
            "menu" => Ok(Self::Menu(rest.parse().map_err(|_| "menu requires a decimal command ID")?)),
            "click" => {
                let values: Vec<_> = rest.split_whitespace().collect();
                if values.len() != 2 {
                    return Err("click requires two signed client coordinates".into());
                }
                Ok(Self::Click(
                    values[0].parse().map_err(|_| "invalid click X")?,
                    values[1].parse().map_err(|_| "invalid click Y")?,
                ))
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
            "reset-metrics" if rest.is_empty() => Ok(Self::ResetMetrics),
            _ => Err(
                "expected click X Y, key/key-down/key-up VK, menus, menu ID, capture PATH, report PATH, reset-metrics or close"
                    .into(),
            ),
        }
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
    input_receipt: u64,
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
                        Command::KeyDown(key) => keys.set(*key, true),
                        Command::KeyUp(key) => keys.set(*key, false),
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
            input_receipt: 0,
            awaiting_until: None,
        })
    }
    pub fn drain(&mut self) -> Vec<Command> {
        self.receiver.fill();
        if let Some(reader) = &self.reader {
            reader.unpark();
        }
        // An interrupt can produce a slice with no guest progress, or stop a
        // partial message dispatch. Keep diagnostics behind a normal guest
        // boundary after input, while new input and close remain responsive.
        let command = if self.awaiting_guest_boundary {
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
            self.input_receipt = 0;
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
    pub fn note_input_receipt(&mut self, receipt: u64) {
        if self.awaiting_guest_boundary {
            self.input_receipt = receipt;
        }
    }
    pub fn awaited_input_receipt(&self) -> Option<u64> {
        self.awaiting_guest_boundary.then_some(self.input_receipt)
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
            input_receipt: 0,
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
        assert_eq!(Command::parse("key-down 0x11"), Ok(Command::KeyDown(17)));
        assert_eq!(Command::parse("key-up 17"), Ok(Command::KeyUp(17)));
        assert_eq!(Command::parse("menus"), Ok(Command::Menus));
        assert_eq!(Command::parse("menu 100"), Ok(Command::Menu(100)));
        assert_eq!(Command::parse("dialogs"), Ok(Command::Dialogs));
        assert_eq!(Command::parse("dialog 1"), Ok(Command::Dialog(1)));
        assert_eq!(Command::parse("dialog-close"), Ok(Command::DialogClose));
        assert_eq!(Command::parse("reset-metrics"), Ok(Command::ResetMetrics));
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
            "garbage",
        ] {
            assert!(Command::parse(line).is_err(), "{line}");
        }
    }
    #[test]
    fn only_input_and_close_commands_request_guest_preemption() {
        for line in ["click 1 2", "key 65", "key-down 17", "key-up 17", "close"] {
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
            input_receipt: 0,
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
            input_receipt: 0,
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
            input_receipt: 0,
            awaiting_until: None,
        };
        sender.send(Command::KeyUp(17)).unwrap();
        sender.send(Command::Capture("frame.png".into())).unwrap();
        sender.send(Command::Report("report.json".into())).unwrap();
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        control.note_input_receipt(4);
        assert_eq!(control.awaited_input_receipt(), Some(4));
        control.awaiting_until = Some(Instant::now());
        assert!(
            matches!(control.drain().as_slice(), [Command::Error(error)] if error.contains("fence timed out"))
        );
        assert_eq!(control.awaited_input_receipt(), None);
        assert_eq!(control.drain(), [Command::Report("report.json".into())]);
        sender.send(Command::KeyUp(17)).unwrap();
        assert_eq!(control.drain(), [Command::KeyUp(17)]);
        assert_eq!(
            control.awaited_input_receipt(),
            Some(0),
            "new or failed input never reuses an old receipt"
        );
    }
}
