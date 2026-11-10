use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, SupportedStreamConfig};

use crate::mixer::Mixer;
use crate::source::CHANNELS;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Finds the default speakers and the format they want audio in.
pub fn default_device() -> Result<(Device, SupportedStreamConfig)> {
    let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
    let config = device.default_output_config()?;
    Ok((device, config))
}

/// Starts pulling audio from the mixer. Playback stops when the returned stream is dropped.
///
/// The mixer is always driven by the speakers' clock, even while `muted`, so that HTTP
/// listeners keep receiving audio.
pub fn start(
    device: &Device,
    config: SupportedStreamConfig,
    mixer: Arc<Mixer>,
    muted: Arc<AtomicBool>,
) -> Result<Stream> {
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config, mixer, muted),
        SampleFormat::I16 => build::<i16>(device, config, mixer, muted),
        SampleFormat::I32 => build::<i32>(device, config, mixer, muted),
        SampleFormat::U16 => build::<u16>(device, config, mixer, muted),
        other => return Err(format!("unsupported output sample format: {other}").into()),
    }?;
    stream.play()?;
    Ok(stream)
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &Device,
    config: SupportedStreamConfig,
    mixer: Arc<Mixer>,
    muted: Arc<AtomicBool>,
) -> Result<Stream> {
    let device_channels = config.channels() as usize;
    let mut mixed: Vec<f32> = Vec::new();

    let stream = device.build_output_stream(
        config.into(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            mixed.resize(data.len() / device_channels * CHANNELS, 0.0);
            mixer.fill(&mut mixed);
            let muted = muted.load(Ordering::Relaxed);

            for (out, frame) in data.chunks_mut(device_channels).zip(mixed.chunks(CHANNELS)) {
                for (channel, sample) in out.iter_mut().enumerate() {
                    let value = match (muted, device_channels, channel) {
                        (true, _, _) => 0.0,
                        (_, 1, _) => (frame[0] + frame[1]) / 2.0,
                        (_, _, 0 | 1) => frame[channel],
                        _ => 0.0,
                    };
                    *sample = T::from_sample(value.clamp(-1.0, 1.0));
                }
            }
        },
        |err| eprintln!("Audio output error: {err}"),
        None,
    )?;
    Ok(stream)
}
