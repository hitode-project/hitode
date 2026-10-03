//! Explicit diagnostic control. The reader never touches guest or window state.
use crate::window::WakeEvent;
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Click(i16, i16),
    Key(u32),
    KeyDown(u32),
    KeyUp(u32),
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
            Self::Click(..) | Self::Key(_) | Self::KeyDown(_) | Self::KeyUp(_) | Self::Close
        )
    }
    fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match command {
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
                "expected click X Y, key/key-down/key-up VK, capture PATH, report PATH, reset-metrics or close"
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

pub struct Control {
    receiver: Receiver<Command>,
    pending: VecDeque<Command>,
    awaiting_guest_boundary: bool,
    input_receipt: u64,
    awaiting_until: Option<Instant>,
}
impl Control {
    pub fn start(
        wake: WakeEvent,
        keys: std::sync::Arc<pal_host::input::KeyState>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let reader_wake = wake.clone();
        std::thread::Builder::new()
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
                    if sender.send(command).is_err() {
                        break;
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
            pending: VecDeque::new(),
            awaiting_guest_boundary: false,
            input_receipt: 0,
            awaiting_until: None,
        })
    }
    pub fn drain(&mut self) -> Vec<Command> {
        loop {
            match self.receiver.try_recv() {
                Ok(command) => self.pending.push_back(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        // An interrupt can produce a slice with no guest progress, or stop a
        // partial message dispatch. Keep diagnostics behind a normal guest
        // boundary after input, while new input and close remain responsive.
        let command = if self.awaiting_guest_boundary {
            let input = self
                .pending
                .iter()
                .position(Command::interrupts_guest)
                .and_then(|index| self.pending.remove(index));
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
                self.pending.pop_front().map(|command| {
                    Command::Error(format!(
                        "input observation fence timed out before {command:?}"
                    ))
                })
            } else {
                None
            }
        } else {
            self.pending.pop_front()
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
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_preserve_paths_and_reject_invalid_packets() {
        assert_eq!(
            Command::parse("click 650 410"),
            Ok(Command::Click(650, 410))
        );
        assert_eq!(Command::parse("key 0x0d"), Ok(Command::Key(13)));
        assert_eq!(Command::parse("key-down 0x11"), Ok(Command::KeyDown(17)));
        assert_eq!(Command::parse("key-up 17"), Ok(Command::KeyUp(17)));
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
        let (sender, receiver) = mpsc::channel();
        let mut control = Control {
            receiver,
            pending: VecDeque::new(),
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
        let (sender, receiver) = mpsc::channel();
        let mut control = Control {
            receiver,
            pending: VecDeque::new(),
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
        let (sender, receiver) = mpsc::channel();
        let mut control = Control {
            receiver,
            pending: VecDeque::new(),
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
