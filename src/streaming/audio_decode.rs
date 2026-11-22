//! Audio decoding for a camera stream: the audio packets of the RTSP session
//! become interleaved stereo `f32` at the output device rate, pushed into an
//! `AudioSink`.
//!
//! Audio is only decoded while a sink is set (the camera is enlarged and not
//! muted); otherwise the packets are dropped without being decoded, so the
//! cameras in the grid cost nothing for sound.

use ffmpeg_next::codec::context::Context;
use ffmpeg_next::codec::Parameters;
use ffmpeg_next::format::sample::Type;
use ffmpeg_next::format::Sample;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::util::frame::audio::Audio as AudioFrame;
use ffmpeg_next::Packet;

use crate::audio::AudioSink;

/// Format the resampler produces: packed `f32`.
const OUTPUT_FORMAT: Sample = Sample::F32(Type::Packed);

/// Extra samples allocated in the resampler output, as margin.
const OUTPUT_MARGIN_SAMPLES: u64 = 32;

/// What a resampler was created for. When the decoded frames change in any
/// of these, the resampler has to be recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResamplerConfig {
    format: Sample,
    layout: ChannelLayout,
    rate: u32,
}

impl ResamplerConfig {
    /// Mono sources stay mono through the resampler and are duplicated into
    /// both channels afterwards: swresample's mono to stereo matrix lowers the
    /// level by 3 dB, which these quiet camera microphones cannot spare.
    fn is_mono(&self) -> bool {
        self.layout.channels() == 1
    }

    fn output_layout(&self) -> ChannelLayout {
        if self.is_mono() {
            ChannelLayout::MONO
        } else {
            ChannelLayout::STEREO
        }
    }
}

/// Audio decoder of one stream connection.
pub struct AudioDecoder {
    parameters: Parameters,
    active: Option<Active>,
    /// Opening or decoding failed; stays off until the audio is turned off
    /// and on again, so a bad codec does not fail on every packet.
    failed: bool,
}

/// Decoder and resampler open for one sink.
struct Active {
    sink: AudioSink,
    decoder: ffmpeg_next::decoder::Audio,
    resampler: Option<(ResamplerConfig, resampling::Context)>,
    /// Decoded frame, reused for every frame.
    decoded: AudioFrame,
    /// Interleaved stereo samples of one frame, reused.
    pcm: Vec<f32>,
}

impl AudioDecoder {
    /// `None` when FFmpeg has no decoder for the stream's codec.
    pub fn new(parameters: &Parameters) -> Option<Self> {
        ffmpeg_next::codec::decoder::find(parameters.id())?;
        Some(Self {
            parameters: parameters.clone(),
            active: None,
            failed: false,
        })
    }

    /// Handles one audio packet: decoded into `sink` when there is one (opening
    /// the decoder on the first packet), dropped when there is none. Failures
    /// turn the audio of this connection off; they never affect the video.
    pub fn process(&mut self, packet: &Packet, sink: Option<&AudioSink>) {
        let Some(sink) = sink else {
            self.active = None;
            self.failed = false;
            return;
        };
        if self.failed {
            return;
        }

        if self
            .active
            .as_ref()
            .is_none_or(|active| !active.sink.same_output(sink))
        {
            match Active::open(&self.parameters, sink.clone()) {
                Ok(active) => self.active = Some(active),
                Err(err) => {
                    tracing::warn!(error = %err, "failed to open the audio decoder; camera audio is off");
                    self.failed = true;
                    return;
                }
            }
        }

        if let Some(active) = &mut self.active {
            if let Err(err) = active.decode(packet) {
                tracing::warn!(error = %err, "failed to decode the camera audio; it is off");
                self.active = None;
                self.failed = true;
            }
        }
    }
}

impl Active {
    fn open(parameters: &Parameters, sink: AudioSink) -> Result<Self, String> {
        let decoder = Context::from_parameters(parameters.clone())
            .map_err(|err| err.to_string())?
            .decoder()
            .audio()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            sink,
            decoder,
            resampler: None,
            decoded: AudioFrame::empty(),
            pcm: Vec::new(),
        })
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|err| err.to_string())?;
        while self.decoder.receive_frame(&mut self.decoded).is_ok() {
            self.convert_and_push()?;
        }
        Ok(())
    }

    /// Resamples `self.decoded` to the output rate and queues it in the sink.
    fn convert_and_push(&mut self) -> Result<(), String> {
        let channels = i32::from(self.decoded.channels());
        if channels == 0 || self.decoded.samples() == 0 || self.decoded.rate() == 0 {
            return Ok(());
        }

        // Some decoders (G.711, for one) leave the channel layout unspecified.
        let mut layout = self.decoded.channel_layout();
        if layout.is_empty() {
            layout = ChannelLayout::default(channels);
            if layout.is_empty() {
                return Err(format!("unsupported number of audio channels: {channels}"));
            }
            self.decoded.set_channel_layout(layout);
        }

        let config = ResamplerConfig {
            format: self.decoded.format(),
            layout,
            rate: self.decoded.rate(),
        };
        let output_rate = self.sink.sample_rate();
        if self.resampler.as_ref().map(|(current, _)| *current) != Some(config) {
            let context = resampling::Context::get(
                config.format,
                config.layout,
                config.rate,
                OUTPUT_FORMAT,
                config.output_layout(),
                output_rate,
            )
            .map_err(|err| err.to_string())?;
            self.resampler = Some((config, context));
        }
        let (_, resampler) = self
            .resampler
            .as_mut()
            .expect("resampler created just above");

        // The output frame has to be allocated with room for everything this
        // input produces (plus what the resampler still holds): `run` would
        // otherwise allocate as many samples as the input has, truncating the
        // conversion when raising the rate (8 kHz to 48 kHz here) and letting
        // the internal delay grow without end.
        let held = resampler
            .delay()
            .map_or(0, |delay| u64::try_from(delay.output).unwrap_or(0));
        let capacity = (self.decoded.samples() as u64 * u64::from(output_rate))
            .div_ceil(u64::from(config.rate))
            + held
            + OUTPUT_MARGIN_SAMPLES;
        let mut converted = AudioFrame::new(
            OUTPUT_FORMAT,
            usize::try_from(capacity).map_err(|err| err.to_string())?,
            config.output_layout(),
        );
        resampler
            .run(&self.decoded, &mut converted)
            .map_err(|err| err.to_string())?;

        let samples = converted.samples();
        if samples == 0 {
            return Ok(());
        }
        let channels_out = if config.is_mono() { 1 } else { 2 };
        let bytes = converted
            .data(0)
            .get(..samples * channels_out * std::mem::size_of::<f32>())
            .ok_or_else(|| "the resampler returned less data than it reported".to_owned())?;

        self.pcm.clear();
        let (words, _) = bytes.as_chunks::<4>();
        let values = words.iter().map(|word| f32::from_ne_bytes(*word));
        if config.is_mono() {
            self.pcm.extend(values.flat_map(|sample| [sample, sample]));
        } else {
            self.pcm.extend(values);
        }
        self.sink.push_stereo(&self.pcm);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// G.711 A-law encoder (ITU-T reference algorithm), to build test audio.
    fn linear_to_alaw(sample: i16) -> u8 {
        const SEGMENT_END: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
        let mut value = i32::from(sample) >> 3;
        let mask: i32 = if value >= 0 {
            0xD5
        } else {
            value = -value - 1;
            0x55
        };
        let segment = SEGMENT_END
            .iter()
            .position(|end| value <= *end)
            .unwrap_or(8);
        if segment >= 8 {
            return (0x7F ^ mask) as u8;
        }
        let shift = if segment < 2 { 1 } else { segment };
        (((segment as i32) << 4 | ((value >> shift) & 0xF)) ^ mask) as u8
    }

    /// WAV file with `samples` as A-law, mono, at `rate`.
    fn write_alaw_wav(path: &Path, samples: &[u8], rate: u32) {
        let data_len = samples.len() as u32;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((4 + 8 + 18 + 8 + 4 + 8 + data_len + (data_len & 1)).to_le_bytes());
        wav.extend(b"WAVE");
        wav.extend(b"fmt ");
        wav.extend(18u32.to_le_bytes());
        wav.extend(6u16.to_le_bytes()); // A-law
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(rate.to_le_bytes());
        wav.extend(rate.to_le_bytes()); // bytes per second
        wav.extend(1u16.to_le_bytes()); // block align
        wav.extend(8u16.to_le_bytes()); // bits per sample
        wav.extend(0u16.to_le_bytes()); // extension size
        wav.extend(b"fact");
        wav.extend(4u32.to_le_bytes());
        wav.extend(data_len.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        wav.extend(samples);
        if !data_len.is_multiple_of(2) {
            wav.push(0);
        }
        std::fs::write(path, wav).expect("failed to write the test wav file");
    }

    /// One second of a 440 Hz tone at 8 kHz, as A-law.
    fn tone_alaw() -> Vec<u8> {
        (0..8000)
            .map(|n| {
                let phase = 2.0 * std::f64::consts::PI * 440.0 * f64::from(n) / 8000.0;
                linear_to_alaw((phase.sin() * 16_000.0) as i16)
            })
            .collect()
    }

    fn temp_wav() -> PathBuf {
        std::env::temp_dir().join(format!(
            "audio_decode_test_{}_{}.wav",
            std::process::id(),
            crate::store::new_camera_id()
        ))
    }

    /// Feeds every audio packet of `path` through `decoder`, with `sink`.
    fn feed_file(path: &Path, sink: Option<&AudioSink>) -> AudioDecoder {
        let mut input = ffmpeg_next::format::input(&path).expect("failed to open the test wav");
        let (index, parameters) = {
            let stream = input
                .streams()
                .best(ffmpeg_next::media::Type::Audio)
                .expect("no audio stream in the test wav");
            (stream.index(), stream.parameters())
        };
        assert_eq!(parameters.id(), ffmpeg_next::codec::Id::PCM_ALAW);
        let mut decoder = AudioDecoder::new(&parameters).expect("no A-law decoder");

        loop {
            let mut packet = Packet::empty();
            match packet.read(&mut input) {
                Ok(()) if packet.stream() == index => decoder.process(&packet, sink),
                Ok(()) => {}
                Err(ffmpeg_next::Error::Eof) => break,
                Err(err) => panic!("failed to read the test wav: {err}"),
            }
        }
        decoder
    }

    #[test]
    fn alaw_encoder_matches_reference_values() {
        // Known G.711 A-law codes: silence and the largest magnitudes.
        assert_eq!(linear_to_alaw(0), 0xD5);
        assert_eq!(linear_to_alaw(-1), 0x55);
        assert_eq!(linear_to_alaw(i16::MAX), 0xAA);
        assert_eq!(linear_to_alaw(i16::MIN), 0x2A);
    }

    #[test]
    fn camera_style_alaw_audio_becomes_stereo_at_the_output_rate() {
        let path = temp_wav();
        write_alaw_wav(&path, &tone_alaw(), 8000);

        let sink = AudioSink::detached_unbounded(48_000);
        feed_file(&path, Some(&sink));
        std::fs::remove_file(&path).ok();
        let pcm = sink.take_all();

        // 1 s at 48 kHz stereo, minus what the resampler still holds back.
        let frames = pcm.len() / 2;
        assert!(pcm.len().is_multiple_of(2));
        assert!(
            (47_500..=48_100).contains(&frames),
            "unexpected number of frames: {frames}"
        );

        // Mono is duplicated into both channels, with no 3 dB loss: the peak
        // is the tone amplitude (16000 / 32768 = 0.488).
        let (pairs, _) = pcm.as_chunks::<2>();
        assert!(pairs.iter().all(|pair| pair[0] == pair[1]));
        let peak = pcm
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!((0.45..=0.53).contains(&peak), "unexpected peak: {peak}");

        // The tone is still 440 Hz (rising zero crossings per second).
        let left: Vec<f32> = pairs.iter().map(|pair| pair[0]).collect();
        let crossings = left
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        assert!(
            (435..=445).contains(&crossings),
            "unexpected frequency: {crossings} Hz"
        );
    }

    #[test]
    fn no_sink_means_the_audio_is_dropped_and_turning_it_on_later_works() {
        let path = temp_wav();
        write_alaw_wav(&path, &tone_alaw(), 8000);

        // Without a sink nothing is decoded or opened.
        let decoder = feed_file(&path, None);
        assert!(decoder.active.is_none());
        assert!(!decoder.failed);

        // Turning it on afterwards decodes from there.
        let sink = AudioSink::detached_unbounded(44_100);
        feed_file(&path, Some(&sink));
        std::fs::remove_file(&path).ok();
        let frames = sink.take_all().len() / 2;
        // 1 s at 44.1 kHz.
        assert!((43_500..=44_300).contains(&frames), "{frames} frames");
    }

    #[test]
    fn switching_the_sink_off_closes_the_decoder() {
        let path = temp_wav();
        write_alaw_wav(&path, &tone_alaw(), 8000);

        let sink = AudioSink::detached_unbounded(48_000);
        let mut decoder = feed_file(&path, Some(&sink));
        assert!(decoder.active.is_some());

        let packet = Packet::copy(&[0xD5; 160]);
        decoder.process(&packet, None);
        assert!(decoder.active.is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mono_stays_mono_through_the_resampler_and_stereo_does_not() {
        let mono = ResamplerConfig {
            format: Sample::I16(Type::Packed),
            layout: ChannelLayout::MONO,
            rate: 8000,
        };
        assert!(mono.is_mono());
        assert_eq!(mono.output_layout(), ChannelLayout::MONO);

        let stereo = ResamplerConfig {
            layout: ChannelLayout::STEREO,
            ..mono
        };
        assert!(!stereo.is_mono());
        assert_eq!(stereo.output_layout(), ChannelLayout::STEREO);
    }
}
