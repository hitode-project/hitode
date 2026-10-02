//! Explicit diagnostic control. The reader never touches guest or window state.
use crate::window::WakeEvent;
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Click(i16, i16),
    Key(u32),
    Capture(PathBuf),
    Report(PathBuf),
    ResetMetrics,
    Close,
    Error(String),
}

impl Command {
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
            "key" => {
                let key = if let Some(hex) = rest.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16)
                } else {
                    rest.parse()
                }
                .map_err(|_| "key requires a decimal or 0x-prefixed virtual key")?;
                if key > 255 {
                    return Err("virtual key must be in 0..=255".into());
                }
                Ok(Self::Key(key))
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
                "expected click X Y, key VK, capture PATH, report PATH, reset-metrics or close"
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
    connected: bool,
    wake: WakeEvent,
}
impl Control {
    pub fn start() -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let wake = WakeEvent::new()?;
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
                    if sender.send(command).is_err() {
                        break;
                    }
                    reader_wake.signal();
                    if failed {
                        break;
                    }
                }
                drop(sender);
                reader_wake.signal();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            receiver,
            pending: VecDeque::new(),
            connected: true,
            wake,
        })
    }
    pub fn drain(&mut self) -> Vec<Command> {
        loop {
            match self.receiver.try_recv() {
                Ok(command) => self.pending.push_back(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.connected = false;
                    break;
                }
            }
        }
        // Give the guest a resumable time slice between diagnostic commands.
        // Input ACKs confirm queuing; guest dispatch may require more than one slice.
        self.pending.pop_front().into_iter().collect()
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn wake(&self) -> &WakeEvent {
        &self.wake
    }
    pub fn wait(&mut self, duration: Option<Duration>) -> bool {
        if self.has_pending() {
            return true;
        }
        if !self.connected {
            std::thread::sleep(duration.unwrap_or(Duration::from_secs(1)));
            return false;
        }
        let result = match duration {
            Some(duration) => self.receiver.recv_timeout(duration),
            None => self
                .receiver
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        match result {
            Ok(command) => {
                self.pending.push_back(command);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => {
                self.connected = false;
                false
            }
        }
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
        assert_eq!(Command::parse("reset-metrics"), Ok(Command::ResetMetrics));
        assert_eq!(
            Command::parse("capture target/a frame.png"),
            Ok(Command::Capture("target/a frame.png".into()))
        );
        for line in [
            "click 1",
            "click 99999 1",
            "key 256",
            "capture",
            "close extra",
            "reset-metrics extra",
            "garbage",
        ] {
            assert!(Command::parse(line).is_err(), "{line}");
        }
    }
}
