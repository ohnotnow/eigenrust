use std::thread;
use std::time::Duration;

use crate::mixer::Mixer;
use crate::source::StreamSource;

const FADE_STEPS: u32 = 30;
const STATIC_HOLD: Duration = Duration::from_millis(500);
const FADE_OUT: Duration = Duration::from_millis(400);

/// The tuning sequence: current station fades into static, the station is swapped while
/// only static is audible, then static fades into the new station.
///
/// `on_swap` is called at the moment of the swap. With nothing playing yet, the static
/// fades up from silence instead.
pub fn tune(mixer: &Mixer, next: StreamSource, crossfade: Duration, on_swap: impl FnOnce()) {
    let half = crossfade / 2;

    if mixer.has_stream() {
        fade(mixer, half, |progress| (1.0 - progress, progress));
    } else {
        fade(mixer, half, |progress| (0.0, progress));
    }

    // Hold the static briefly, like the knob travelling between stations.
    thread::sleep(STATIC_HOLD);
    mixer.set_stream(next);
    on_swap();

    fade(mixer, half, |progress| (progress, 1.0 - progress));
}

/// Fades the current station to silence and lets go of it.
pub fn fade_out(mixer: &Mixer) {
    fade(mixer, FADE_OUT, |progress| (1.0 - progress, 0.0));
    mixer.clear_stream();
}

/// Steps the (stream, static) volumes along `curve` as progress runs from 0 to 1.
fn fade(mixer: &Mixer, duration: Duration, curve: impl Fn(f32) -> (f32, f32)) {
    for step in 0..=FADE_STEPS {
        let (stream, static_) = curve(step as f32 / FADE_STEPS as f32);
        mixer.set_volumes(stream, static_);
        thread::sleep(duration / FADE_STEPS);
    }
}
