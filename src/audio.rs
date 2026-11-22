//! Audio output: plays the sound of the enlarged camera through the default
//! Windows output device (WASAPI, via `cpal`).
//!
//! The stream tasks decode and resample the camera audio to stereo `f32` at
//! the device rate and push it into an `AudioSink`; the device callback pulls
//! it from the same buffer. The buffer keeps the latency low: playback only
//! starts once a little sound is queued (so it does not stutter at the start),
//! and when the camera clock runs ahead of the device the oldest sound is
//! dropped instead of the delay growing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Sound that must be queued before playback starts (and restarts after the
/// buffer runs dry). Cameras send their audio in chunky packets (about 64 ms
/// each), so this keeps a couple of them queued as margin against network
/// jitter.
const PREBUFFER: Duration = Duration::from_millis(200);

/// Most sound kept queued. Past this, the oldest is dropped, so a stalled
/// device or a fast camera clock never turns into seconds of delay.
const MAX_BUFFERED: Duration = Duration::from_millis(800);

/// Highest amplitude gain, at full volume: +12 dB. Camera microphones are
/// quiet (the ones tested peak around -20 dB with ambient sound), so the
/// slider needs headroom above the original level.
const MAX_GAIN: f32 = 4.0;

/// Default volume (0 to 1) of the enlarged camera's sound: the half way point,
/// which plays the camera audio at its original level.
pub const DEFAULT_VOLUME: f32 = 0.5;

/// Interleaved stereo (L, R) samples waiting to be played.
struct Ring {
    samples: VecDeque<f32>,
    /// Playback is running (the pre-buffer was reached and no underrun since).
    primed: bool,
}

impl Ring {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            primed: false,
        }
    }

    /// Queues interleaved stereo samples, keeping at most `max_samples`: if
    /// more would be queued, the oldest are dropped.
    fn push(&mut self, stereo: &[f32], max_samples: usize) {
        self.samples.extend(stereo);
        let excess = self.samples.len().saturating_sub(max_samples);
        if excess > 0 {
            self.samples.drain(..excess);
        }
    }

    /// Fills `out` (interleaved, `channels` per frame) from the queue, scaled
    /// by `gain` and kept within -1 to 1. Stereo goes to the first two
    /// channels (mono gets the average, extra channels stay silent). Writes
    /// silence until `prebuffer` samples are queued, and again after the queue
    /// runs dry.
    fn fill(&mut self, out: &mut [f32], channels: usize, gain: f32, prebuffer: usize) {
        out.fill(0.0);
        if channels == 0 {
            return;
        }
        if !self.primed {
            if self.samples.len() < prebuffer.max(2) {
                return;
            }
            self.primed = true;
        }

        for frame in out.chunks_exact_mut(channels) {
            let (Some(left), Some(right)) =
                (self.samples.front().copied(), self.samples.get(1).copied())
            else {
                // Ran dry: back to waiting for the pre-buffer.
                self.primed = false;
                return;
            };
            self.samples.drain(..2);
            let (left, right) = (left * gain, right * gain);
            if channels == 1 {
                frame[0] = ((left + right) / 2.0).clamp(-1.0, 1.0);
            } else {
                frame[0] = left.clamp(-1.0, 1.0);
                frame[1] = right.clamp(-1.0, 1.0);
            }
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.primed = false;
    }
}

/// State shared between the stream tasks (producers) and the device callback.
struct Shared {
    ring: Mutex<Ring>,
    /// Device sample rate; the stream tasks resample to it.
    sample_rate: u32,
    /// Volume (0 to 1) as `f32` bits.
    volume: AtomicU32,
    max_samples: usize,
    prebuffer_samples: usize,
}

impl Shared {
    fn new(sample_rate: u32) -> Self {
        let per_second = sample_rate as f64 * 2.0;
        Self {
            ring: Mutex::new(Ring::new()),
            sample_rate,
            volume: AtomicU32::new(DEFAULT_VOLUME.to_bits()),
            max_samples: (per_second * MAX_BUFFERED.as_secs_f64()) as usize,
            prebuffer_samples: (per_second * PREBUFFER.as_secs_f64()) as usize,
        }
    }

    /// Volume as an amplitude gain, on a squared curve so the slider feels
    /// even to the ear: 0 is silent, `DEFAULT_VOLUME` (half way) is the
    /// original level (gain 1) and 1 is `MAX_GAIN`.
    fn gain(&self) -> f32 {
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed)).clamp(0.0, 1.0);
        let scaled = volume * MAX_GAIN.sqrt();
        scaled * scaled
    }

    fn fill(&self, out: &mut [f32], channels: usize) {
        let gain = self.gain();
        let mut ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        ring.fill(out, channels, gain, self.prebuffer_samples);
    }
}

/// Where the stream tasks send the decoded sound. Cheap to clone and share
/// across threads.
#[derive(Clone)]
pub struct AudioSink {
    shared: Arc<Shared>,
}

impl AudioSink {
    /// Sample rate the sound must be resampled to before `push_stereo`.
    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    /// Queues interleaved stereo (L, R) `f32` samples at `sample_rate`.
    pub fn push_stereo(&self, samples: &[f32]) {
        let mut ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ring.push(samples, self.shared.max_samples);
    }

    /// Both sinks feed the same output.
    pub fn same_output(&self, other: &AudioSink) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// A sink with no device behind it that keeps everything pushed (tests).
    #[cfg(test)]
    pub fn detached_unbounded(sample_rate: u32) -> Self {
        let mut shared = Shared::new(sample_rate);
        shared.max_samples = usize::MAX;
        Self {
            shared: Arc::new(shared),
        }
    }

    /// Takes everything queued so far (tests).
    #[cfg(test)]
    pub fn take_all(&self) -> Vec<f32> {
        let mut ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ring.samples.drain(..).collect()
    }
}

/// The open output device. Lives on the UI thread; the stream tasks only see
/// the `AudioSink`.
pub struct AudioOutput {
    shared: Arc<Shared>,
    /// `None` only for the device-less output used in tests.
    stream: Option<cpal::Stream>,
}

impl AudioOutput {
    /// Opens the default output device. The stream starts paused; see
    /// `set_active`. Takes a few tens of milliseconds.
    pub fn open() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no audio output device".to_owned())?;
        let supported = device
            .default_output_config()
            .map_err(|err| err.to_string())?;

        let shared = Arc::new(Shared::new(supported.sample_rate()));
        let channels = usize::from(supported.channels());
        let config = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, config, channels, &shared),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, config, channels, &shared),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, config, channels, &shared),
            cpal::SampleFormat::I32 => build_stream::<i32>(&device, config, channels, &shared),
            other => Err(format!("unsupported audio sample format: {other}")),
        }?;
        // Some backends start playing as soon as the stream is built.
        let _ = stream.pause();

        Ok(Self {
            shared,
            stream: Some(stream),
        })
    }

    /// An output with no device, for tests.
    #[cfg(test)]
    pub fn detached(sample_rate: u32) -> Self {
        Self {
            shared: Arc::new(Shared::new(sample_rate)),
            stream: None,
        }
    }

    /// Sink for the stream tasks.
    pub fn sink(&self) -> AudioSink {
        AudioSink {
            shared: self.shared.clone(),
        }
    }

    /// Volume from 0 (silent) to 1 (full).
    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Starts or pauses the device. While paused (no camera enlarged, or
    /// muted) the app holds no active audio session, and whatever was queued
    /// is discarded so the next start does not replay old sound.
    pub fn set_active(&self, active: bool) {
        self.shared
            .ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        if let Some(stream) = &self.stream {
            let result = if active {
                stream.play()
            } else {
                stream.pause()
            };
            if let Err(err) = result {
                tracing::warn!(error = %err, active, "failed to change the audio output state");
            }
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let shared = shared.clone();
    // Reused by every callback, so the audio thread does not allocate.
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                scratch.resize(data.len(), 0.0);
                shared.fill(&mut scratch, channels);
                for (out, sample) in data.iter_mut().zip(&scratch) {
                    *out = T::from_sample(*sample);
                }
            },
            |err| tracing::debug!(error = %err, "audio output stream event"),
            None,
        )
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interleaved stereo with `frames` frames: left = `left`, right = `right`.
    fn stereo(frames: usize, left: f32, right: f32) -> Vec<f32> {
        (0..frames).flat_map(|_| [left, right]).collect()
    }

    #[test]
    fn silent_until_the_prebuffer_is_reached_then_plays() {
        let mut ring = Ring::new();
        let mut out = [1.0_f32; 8];

        // 4 frames queued, 10 needed: still silent, and nothing is consumed.
        ring.push(&stereo(4, 0.5, -0.5), usize::MAX);
        ring.fill(&mut out, 2, 1.0, 20);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert_eq!(ring.samples.len(), 8);

        // Past the pre-buffer it plays what was queued.
        ring.push(&stereo(6, 0.5, -0.5), usize::MAX);
        ring.fill(&mut out, 2, 1.0, 20);
        assert_eq!(out, [0.5, -0.5, 0.5, -0.5, 0.5, -0.5, 0.5, -0.5]);
    }

    #[test]
    fn underrun_plays_what_is_left_then_waits_for_the_prebuffer_again() {
        let mut ring = Ring::new();
        ring.push(&stereo(3, 0.25, 0.25), usize::MAX);
        let mut out = [9.0_f32; 10]; // 5 frames, only 3 queued
        ring.fill(&mut out, 2, 1.0, 2);
        assert_eq!(
            out,
            [0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0]
        );

        // Dry: new sound below the pre-buffer is held back.
        ring.push(&stereo(1, 0.9, 0.9), usize::MAX);
        let mut out = [9.0_f32; 4];
        ring.fill(&mut out, 2, 1.0, 20);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn queue_is_capped_dropping_the_oldest_sound() {
        let mut ring = Ring::new();
        // Room for 4 samples (2 frames): the first frame is dropped.
        ring.push(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0], 4);
        let kept: Vec<f32> = ring.samples.iter().copied().collect();
        assert_eq!(kept, [2.0, 2.0, 3.0, 3.0]);

        // Further pushes keep dropping the oldest.
        ring.push(&[4.0, 4.0], 4);
        let kept: Vec<f32> = ring.samples.iter().copied().collect();
        assert_eq!(kept, [3.0, 3.0, 4.0, 4.0]);
    }

    #[test]
    fn stereo_is_mapped_to_any_number_of_device_channels() {
        let sound = stereo(2, 0.5, -0.25);

        // Mono: the average of both channels.
        let mut ring = Ring::new();
        ring.push(&sound, usize::MAX);
        let mut mono = [0.0_f32; 2];
        ring.fill(&mut mono, 1, 1.0, 2);
        assert_eq!(mono, [0.125, 0.125]);

        // 6 channels (5.1): left and right in the first two, the rest silent.
        let mut ring = Ring::new();
        ring.push(&sound, usize::MAX);
        let mut surround = [9.0_f32; 12];
        ring.fill(&mut surround, 6, 1.0, 2);
        assert_eq!(
            surround,
            [0.5, -0.25, 0.0, 0.0, 0.0, 0.0, 0.5, -0.25, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn gain_scales_and_output_never_clips() {
        let mut ring = Ring::new();
        ring.push(&stereo(2, 0.5, 0.5), usize::MAX);
        let mut out = [0.0_f32; 4];
        ring.fill(&mut out, 2, 0.5, 2);
        assert_eq!(out, [0.25; 4]);

        let mut ring = Ring::new();
        ring.push(&stereo(2, 1.4, -1.4), usize::MAX);
        let mut out = [0.0_f32; 4];
        ring.fill(&mut out, 2, 1.0, 2);
        assert_eq!(out, [1.0, -1.0, 1.0, -1.0]);
    }

    #[test]
    fn volume_maps_to_a_gain_with_headroom_above_the_original_level() {
        let output = AudioOutput::detached(48_000);
        let gain = |volume: f32| {
            output.set_volume(volume);
            output.shared.gain()
        };
        assert_eq!(gain(0.0), 0.0);
        // Half way (the default) plays the camera audio at its original level.
        assert!((gain(DEFAULT_VOLUME) - 1.0).abs() < 1e-6);
        // Full volume is +12 dB (4x the amplitude).
        assert!((gain(1.0) - MAX_GAIN).abs() < 1e-6);
        // The curve is increasing.
        assert!(gain(0.25) < gain(0.5) && gain(0.5) < gain(0.75));
        // Out-of-range values are clamped.
        assert!((gain(7.0) - MAX_GAIN).abs() < 1e-6);
        assert_eq!(gain(-1.0), 0.0);
    }

    #[test]
    fn sink_feeds_the_output_and_set_active_discards_queued_sound() {
        let output = AudioOutput::detached(48_000);
        let sink = output.sink();
        assert!(sink.same_output(&output.sink()));
        assert_eq!(sink.sample_rate(), 48_000);

        sink.push_stereo(&stereo(100, 0.1, 0.1));
        let mut out = vec![0.0_f32; 200];
        output.shared.fill(&mut out, 2);
        // 100 frames are below the 200 ms pre-buffer (9600 frames): silent.
        assert!(out.iter().all(|sample| *sample == 0.0));

        output.set_active(false);
        assert!(sink.take_all().is_empty());

        // A sink from another output is not the same one.
        assert!(!sink.same_output(&AudioOutput::detached(48_000).sink()));
    }

    #[test]
    fn limits_follow_the_sample_rate() {
        let shared = Shared::new(48_000);
        // 800 ms and 200 ms of interleaved stereo.
        assert_eq!(shared.max_samples, 76_800);
        assert_eq!(shared.prebuffer_samples, 19_200);
    }
}
