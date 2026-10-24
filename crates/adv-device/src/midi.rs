//! General MIDI synthesis on the execution owner. PCM enters the ordinary
//! mixer queue; the audio callback never loads fonts or processes MIDI events.
use oxisynth::{MidiEvent as Event, SoundFont, Synth as Engine, SynthDescriptor};
use std::io::Cursor;

pub const SAMPLE_RATE: u32 = 44100;

#[derive(Clone, Copy)]
pub enum MidiEvent {
    Short(u32),
    Reset,
    Silence,
    Volume(u16),
}
impl MidiEvent {
    /// Universal GM, GS and XG resets, and universal master volume. Unknown
    /// manufacturer messages are accepted without changing a GM synthesizer.
    pub fn sysex(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0xf0, 0x7e, _, 9, 1..=3, 0xf7]
            | [0xf0, 0x41, _, 0x42, 0x12, 0x40, 0, 0x7f, 0, 0x41, 0xf7]
            | [0xf0, 0x43, 0x10..=0x1f, 0x4c, 0, 0, 0x7e, 0, 0xf7] => Some(Self::Reset),
            [0xf0, 0x7f, _, 4, 1, low, high, 0xf7] => Some(Self::Volume(
                u16::from(*low & 127) | (u16::from(*high & 127) << 7),
            )),
            _ => None,
        }
    }
}

pub struct Synth {
    engine: Engine,
}
impl Synth {
    pub fn new() -> Result<Self, String> {
        let font = SoundFont::load(&mut Cursor::new(include_bytes!(
            "../../../vendor/fluidr3mono/FluidR3Mono_GM.sf3"
        )))
        .map_err(|error| format!("MIDI SoundFont: {error:?}"))?;
        let mut engine = Engine::new(SynthDescriptor {
            polyphony: 128,
            sample_rate: SAMPLE_RATE as f32,
            ..Default::default()
        })
        .map_err(|error| format!("MIDI synthesizer: {error:?}"))?;
        engine.add_font(font, true);
        Ok(Self { engine })
    }
    pub fn event(&mut self, event: MidiEvent) -> Result<(), String> {
        let event = match event {
            MidiEvent::Short(packed) => {
                let status = packed as u8;
                let channel = status & 15;
                let first = ((packed >> 8) & 127) as u8;
                let second = ((packed >> 16) & 127) as u8;
                match status & 0xf0 {
                    0x80 => Event::NoteOff {
                        channel,
                        key: first,
                    },
                    0x90 if second == 0 => Event::NoteOff {
                        channel,
                        key: first,
                    },
                    0x90 => Event::NoteOn {
                        channel,
                        key: first,
                        vel: second,
                    },
                    0xa0 => Event::PolyphonicKeyPressure {
                        channel,
                        key: first,
                        value: second,
                    },
                    0xb0 => Event::ControlChange {
                        channel,
                        ctrl: first,
                        value: second,
                    },
                    0xc0 => Event::ProgramChange {
                        channel,
                        program_id: first,
                    },
                    0xd0 => Event::ChannelPressure {
                        channel,
                        value: first,
                    },
                    0xe0 => Event::PitchBend {
                        channel,
                        value: u16::from(first) | (u16::from(second) << 7),
                    },
                    _ if status == 0xff => Event::SystemReset,
                    _ => return Ok(()),
                }
            }
            MidiEvent::Reset => Event::SystemReset,
            MidiEvent::Silence => {
                for channel in 0..16 {
                    self.engine
                        .send_event(Event::ControlChange {
                            channel,
                            ctrl: 64,
                            value: 0,
                        })
                        .map_err(|e| format!("{e:?}"))?;
                    self.engine
                        .send_event(Event::AllSoundOff { channel })
                        .map_err(|e| format!("{e:?}"))?;
                }
                return Ok(());
            }
            MidiEvent::Volume(volume) => {
                self.engine.set_gain(0.2 * f32::from(volume) / 16383.0);
                return Ok(());
            }
        };
        self.engine
            .send_event(event)
            .map_err(|error| format!("MIDI event: {error:?}"))?;
        if matches!(event, Event::SystemReset) {
            self.engine.reset_program();
            self.engine.set_gain(0.2);
        }
        Ok(())
    }
    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut samples = vec![0.0; frames * 2];
        self.engine.write(&mut samples[..]);
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-AUD-6
    #[test]
    fn bundled_bank_has_all_gm_programs_and_drums_and_renders_notes() {
        let mut synth = Synth::new().unwrap();
        for program in 0..128 {
            synth
                .event(MidiEvent::Short(0xc0 | (program << 8)))
                .unwrap();
            assert_eq!(synth.engine.channel_preset(0).unwrap().num(), program);
        }
        synth.event(MidiEvent::Short(0xc0)).unwrap();
        synth.event(MidiEvent::Short(0x0064_4590)).unwrap();
        let samples = synth.render(4410);
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(samples.iter().any(|v| v.abs() > 0.001));
        synth.event(MidiEvent::Short(0x0064_2699)).unwrap();
        assert_eq!(synth.engine.channel_preset(9).unwrap().banknum(), 128);
        synth.event(MidiEvent::Reset).unwrap();
        synth.render(64); // Drain the synth's already generated block.
        assert!(synth.render(441).iter().all(|v| v.abs() < 0.00001));
        synth.event(MidiEvent::Volume(0)).unwrap();
        synth.event(MidiEvent::Short(0x0064_4590)).unwrap();
        assert!(synth.render(441).iter().all(|v| v.abs() < 0.00001));
    }
}
