//! Audio sources. Everything here produces interleaved stereo `f32` samples at the engine rate.

use std::collections::VecDeque;
use std::error::Error;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};

pub const CHANNELS: usize = 2;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const BUFFERED_SECONDS: usize = 2;
const SILENCE_PEAK: f32 = 0.01;

/// The looping bed of static that plays between stations.
pub struct StaticSource {
    samples: Vec<f32>,
    cursor: usize,
}

impl StaticSource {
    pub fn new(mp3: &'static [u8], rate: u32) -> Result<Self, DecodeError> {
        let mut samples = Vec::new();
        decode(Box::new(Cursor::new(mp3)), &Hint::new(), rate, |chunk| {
            samples.extend_from_slice(chunk);
            true
        })?;
        if samples.is_empty() {
            return Err(DecodeError::DecodeError("static file contains no audio"));
        }
        Ok(Self { samples, cursor: 0 })
    }

    pub fn add_to(&mut self, out: &mut [f32], volume: f32) {
        for sample in out {
            *sample += self.samples[self.cursor] * volume;
            self.cursor = (self.cursor + 1) % self.samples.len();
        }
    }
}

struct Shared {
    buffer: Mutex<VecDeque<f32>>,
    stop: AtomicBool,
    finished: AtomicBool,
    /// What the station calls itself, if its server says.
    name: Mutex<Option<String>>,
}

/// A live internet radio stream, decoded on its own thread into a small buffer.
pub struct StreamSource {
    shared: Arc<Shared>,
}

impl StreamSource {
    pub fn open(url: &str, rate: u32) -> Self {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            name: Mutex::new(None),
        });
        let (url, thread_shared) = (url.to_string(), shared.clone());
        thread::spawn(move || {
            if let Err(e) = receive(&url, rate, &thread_shared) {
                println!("Stream stopped ({url}): {e}");
            }
            thread_shared.finished.store(true, Ordering::Relaxed);
        });
        Self { shared }
    }

    /// Waits until the stream has produced something louder than silence.
    pub fn wait_for_audio(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let finished = self.is_finished();
            let audible = self.shared.buffer.lock().unwrap().iter().any(|s| s.abs() > SILENCE_PEAK);
            if audible {
                return true;
            }
            if finished || Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The station's own name for itself, known once it has connected (and only if it says).
    pub fn name(&self) -> Option<String> {
        self.shared.name.lock().unwrap().clone()
    }

    /// True once the stream has ended or failed, and nothing more will arrive.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    /// Mixes buffered audio into `out`. If the buffer runs dry the rest is left as it was.
    pub fn add_to(&self, out: &mut [f32], volume: f32) {
        let mut buffer = self.shared.buffer.lock().unwrap();
        let available = out.len().min(buffer.len());
        for (sample, decoded) in out.iter_mut().zip(buffer.drain(..available)) {
            *sample += decoded * volume;
        }
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn receive(url: &str, rate: u32, shared: &Shared) -> Result<(), Box<dyn Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .build()
        .into();
    let response = agent.get(url).call()?;

    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
    let mut hint = Hint::new();
    if let Some(content_type) = header("content-type") {
        hint.mime_type(content_type);
    }
    // Icecast and SHOUTcast servers announce the station's name in this header.
    *shared.name.lock().unwrap() =
        header("icy-name").map(str::trim).filter(|name| !name.is_empty()).map(str::to_string);

    let reader = ReadOnlySource::new(response.into_body().into_reader());
    let capacity = rate as usize * CHANNELS * BUFFERED_SECONDS;
    decode(Box::new(reader), &hint, rate, |chunk| {
        // Wait for the listener to catch up rather than dropping audio.
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return false;
            }
            let mut buffer = shared.buffer.lock().unwrap();
            if buffer.len() < capacity {
                buffer.extend(chunk);
                return true;
            }
            drop(buffer);
            thread::sleep(Duration::from_millis(20));
        }
    })?;
    Ok(())
}

/// Decodes `source` to stereo at `out_rate`, handing each chunk to `sink` until it returns false.
fn decode(
    source: Box<dyn MediaSource>,
    hint: &Hint,
    out_rate: u32,
    mut sink: impl FnMut(&[f32]) -> bool,
) -> Result<(), DecodeError> {
    let stream = MediaSourceStream::new(source, Default::default());
    let mut format =
        symphonia::default::get_probe().probe(hint, stream, Default::default(), Default::default())?;

    let mut decoded: Vec<f32> = Vec::new();
    let mut stereo: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    let mut resampler = Resampler::new(out_rate);

    // Each pass handles one track. Chained streams (a new track per song) come round again.
    loop {
        let track = format
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::Unsupported("no audio track"))?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or(DecodeError::Unsupported("no audio codec parameters"))?;
        let mut decoder =
            symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?;

        loop {
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(()),
                Err(DecodeError::ResetRequired) => break,
                Err(e) => return Err(e),
            };
            if packet.track_id != track_id {
                continue;
            }
            let audio = match decoder.decode(&packet) {
                Ok(audio) => audio,
                // A corrupt packet is skipped, but the sink still gets a say in whether to go on.
                Err(DecodeError::DecodeError(_)) if sink(&[]) => continue,
                Err(DecodeError::DecodeError(_)) => return Ok(()),
                Err(DecodeError::ResetRequired) => break,
                Err(e) => return Err(e),
            };

            let in_rate = audio.spec().rate();
            let in_channels = audio.spec().channels().count();
            decoded.resize(audio.samples_interleaved(), 0.0);
            audio.copy_to_slice_interleaved(&mut decoded);
            to_stereo(&decoded, in_channels, &mut stereo);
            resampler.process(in_rate, &stereo, &mut resampled);
            if !sink(&resampled) {
                return Ok(());
            }
        }
    }
}

fn to_stereo(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    for frame in input.chunks_exact(channels) {
        out.push(frame[0]);
        out.push(frame[if channels > 1 { 1 } else { 0 }]);
    }
}

/// Linear-interpolation resampler for interleaved stereo. Plain, but fine for radio.
pub(crate) struct Resampler {
    out_rate: u32,
    /// Position of the next output frame, measured in input frames from `previous`.
    position: f64,
    previous: [f32; CHANNELS],
}

impl Resampler {
    pub(crate) fn new(out_rate: u32) -> Self {
        Self { out_rate, position: 0.0, previous: [0.0; CHANNELS] }
    }

    /// Replaces `out` with `input` converted to the output rate. Matching rates pass through.
    pub(crate) fn process(&mut self, in_rate: u32, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        if in_rate == self.out_rate {
            out.extend_from_slice(input);
            return;
        }

        let frames = input.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        let step = in_rate as f64 / self.out_rate as f64;
        let previous = self.previous;
        // Frame 0 is the last frame of the previous call, so interpolation spans the join.
        let frame = |index: usize| match index {
            0 => previous,
            _ => [input[(index - 1) * CHANNELS], input[(index - 1) * CHANNELS + 1]],
        };

        while self.position < frames as f64 {
            let index = self.position as usize;
            let fraction = (self.position - index as f64) as f32;
            let (a, b) = (frame(index), frame(index + 1));
            out.push(a[0] + (b[0] - a[0]) * fraction);
            out.push(a[1] + (b[1] - a[1]) * fraction);
            self.position += step;
        }
        self.position -= frames as f64;
        self.previous = frame(frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated_and_extra_channels_are_dropped() {
        let mut out = Vec::new();

        to_stereo(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);

        to_stereo(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 3, &mut out);
        assert_eq!(out, [0.1, 0.2, 0.4, 0.5]);
    }

    #[test]
    fn matching_rates_pass_straight_through() {
        let mut resampler = Resampler::new(44100);
        let mut out = Vec::new();

        resampler.process(44100, &[0.1, 0.2, 0.3, 0.4], &mut out);

        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn resampling_keeps_duration_across_chunk_boundaries() {
        let mut resampler = Resampler::new(48000);
        let mut out = Vec::new();
        let mut produced = 0;

        // One second of 22.05kHz audio, fed in uneven chunks.
        let second = vec![0.5f32; 22050 * CHANNELS];
        for chunk in second.chunks(1234 * CHANNELS) {
            resampler.process(22050, chunk, &mut out);
            produced += out.len() / CHANNELS;
        }

        assert!((47999..=48001).contains(&produced), "produced {produced} frames");
    }

    #[test]
    fn upsampling_interpolates_between_samples() {
        let mut resampler = Resampler::new(2);
        let mut out = Vec::new();

        resampler.process(1, &[1.0, 1.0, 3.0, 3.0], &mut out);

        // Starts from the silent "previous" frame, then halfway points between inputs.
        assert_eq!(out, [0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn the_bundled_static_decodes_and_loops() {
        let mut source =
            StaticSource::new(include_bytes!("../assets/static.mp3"), 44100).unwrap();
        let length = source.samples.len();
        assert!(length > 44100 * CHANNELS, "expected more than a second of static");

        let mut out = vec![0.0; length + 10];
        source.add_to(&mut out, 1.0);

        assert_eq!(source.cursor, 10);
        assert!(out.iter().any(|s| s.abs() > SILENCE_PEAK));
    }
}
