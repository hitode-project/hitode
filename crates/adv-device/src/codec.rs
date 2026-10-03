//! Synchronous packet decoding. No guest addresses, handles, API flags or machine state.
//! Every production format uses the same Symphonia registry.
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{
    CODEC_TYPE_ADPCM_IMA_WAV, CODEC_TYPE_ADPCM_MS, CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_U8,
    CodecParameters, Decoder as SymphoniaDecoder, DecoderOptions,
};
use symphonia::core::formats::Packet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    PcmU8,
    PcmS16Le,
    MicrosoftAdpcm,
    ImaAdpcm,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmEncoding {
    U8,
    S16Le,
}

#[derive(Clone, Copy, Debug)]
pub struct DecoderConfig {
    pub codec: Codec,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub frames_per_block: u16,
    pub max_frames_per_packet: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    UnsupportedFormat,
    InvalidPacket,
}
impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedFormat => "unsupported codec format",
            Self::InvalidPacket => "invalid compressed packet",
        })
    }
}
impl std::error::Error for DecodeError {}

pub struct Decoder {
    decoder: Box<dyn SymphoniaDecoder>,
    timestamp: u64,
}
impl Decoder {
    pub fn new(config: DecoderConfig) -> Result<Self, DecodeError> {
        let channels = match config.channels {
            1 => Channels::FRONT_LEFT,
            2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
            _ => return Err(DecodeError::UnsupportedFormat),
        };
        let codec = match config.codec {
            Codec::PcmU8 => CODEC_TYPE_PCM_U8,
            Codec::PcmS16Le => CODEC_TYPE_PCM_S16LE,
            Codec::MicrosoftAdpcm => CODEC_TYPE_ADPCM_MS,
            Codec::ImaAdpcm => CODEC_TYPE_ADPCM_IMA_WAV,
        };
        let mut parameters = CodecParameters::new();
        parameters
            .for_codec(codec)
            .with_sample_rate(config.sample_rate)
            .with_channels(channels)
            .with_bits_per_sample(u32::from(config.bits_per_sample))
            .with_frames_per_block(u64::from(config.frames_per_block))
            .with_max_frames_per_packet(config.max_frames_per_packet as u64);
        let decoder = symphonia::default::get_codecs()
            .make(&parameters, &DecoderOptions::default())
            .map_err(|_| DecodeError::UnsupportedFormat)?;
        Ok(Self {
            decoder,
            timestamp: 0,
        })
    }

    pub fn reset(&mut self) {
        self.decoder.reset();
        self.timestamp = 0;
    }

    /// Append decoded interleaved samples to caller-owned storage, avoiding an extra copy.
    pub fn decode_into(
        &mut self,
        data: &[u8],
        frames: u64,
        encoding: PcmEncoding,
        output: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let packet = Packet::new_from_slice(0, self.timestamp, frames, data);
        let audio = self
            .decoder
            .decode(&packet)
            .map_err(|_| DecodeError::InvalidPacket)?;
        match encoding {
            PcmEncoding::S16Le => {
                let mut buffer = SampleBuffer::<i16>::new(audio.capacity() as u64, *audio.spec());
                buffer.copy_interleaved_ref(audio);
                for sample in buffer.samples() {
                    output.extend_from_slice(&sample.to_le_bytes());
                }
            }
            PcmEncoding::U8 => {
                let mut buffer = SampleBuffer::<u8>::new(audio.capacity() as u64, *audio.spec());
                buffer.copy_interleaved_ref(audio);
                output.extend_from_slice(buffer.samples());
            }
        }
        self.timestamp += frames;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_conversion_and_reset_preserve_exact_signed_samples() {
        let mut decoder = Decoder::new(DecoderConfig {
            codec: Codec::PcmU8,
            sample_rate: 8000,
            channels: 1,
            bits_per_sample: 8,
            frames_per_block: 1,
            max_frames_per_packet: 8,
        })
        .unwrap();
        let mut output = Vec::new();
        decoder
            .decode_into(&[0, 128, 255], 3, PcmEncoding::S16Le, &mut output)
            .unwrap();
        assert_eq!(
            output,
            [-32768i16, 0, 32512]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(decoder.timestamp, 3);
        decoder.reset();
        assert_eq!(decoder.timestamp, 0);
        output.clear();
        decoder
            .decode_into(&[0, 128, 255], 3, PcmEncoding::U8, &mut output)
            .unwrap();
        assert_eq!(output, [0, 128, 255]);
    }
}
