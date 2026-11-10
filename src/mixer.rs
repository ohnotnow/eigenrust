use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use crate::source::{StaticSource, StreamSource};

/// How many chunks an HTTP listener may fall behind before audio is dropped for them.
const LISTENER_QUEUE: usize = 256;

/// A chunk of mixed audio for HTTP listeners: interleaved stereo at the engine rate.
pub type MixChunk = Arc<[f32]>;

struct State {
    static_source: StaticSource,
    stream: Option<StreamSource>,
    stream_volume: f32,
    static_volume: f32,
    listeners: Vec<SyncSender<MixChunk>>,
}

/// Mixes the current station with the static bed.
pub struct Mixer {
    state: Mutex<State>,
}

impl Mixer {
    pub fn new(static_source: StaticSource) -> Self {
        Self {
            state: Mutex::new(State {
                static_source,
                stream: None,
                stream_volume: 0.0,
                static_volume: 0.0,
                listeners: Vec::new(),
            }),
        }
    }

    /// Fills `out` with the next stretch of mixed audio and passes a copy to any listeners.
    pub fn fill(&self, out: &mut [f32]) {
        let mut state = self.state.lock().unwrap();
        let (stream_volume, static_volume) = (state.stream_volume, state.static_volume);

        out.fill(0.0);
        state.static_source.add_to(out, static_volume);
        if let Some(stream) = &state.stream {
            stream.add_to(out, stream_volume);
        }

        if !state.listeners.is_empty() {
            let chunk = MixChunk::from(&*out);
            state.listeners.retain(|listener| {
                !matches!(listener.try_send(chunk.clone()), Err(TrySendError::Disconnected(_)))
            });
        }
    }

    pub fn set_stream(&self, stream: StreamSource) {
        self.state.lock().unwrap().stream = Some(stream);
    }

    pub fn clear_stream(&self) {
        self.state.lock().unwrap().stream = None;
    }

    /// Drops every HTTP listener, which ends their connections.
    pub fn disconnect_listeners(&self) {
        self.state.lock().unwrap().listeners.clear();
    }

    pub fn set_volumes(&self, stream: f32, static_: f32) {
        let mut state = self.state.lock().unwrap();
        state.stream_volume = stream;
        state.static_volume = static_;
    }

    pub fn has_stream(&self) -> bool {
        self.state.lock().unwrap().stream.is_some()
    }

    pub fn stream_finished(&self) -> bool {
        self.state.lock().unwrap().stream.as_ref().is_some_and(StreamSource::is_finished)
    }

    /// Registers a listener for the mixed audio. Dropping the receiver unsubscribes it.
    pub fn subscribe(&self) -> Receiver<MixChunk> {
        let (sender, receiver) = sync_channel(LISTENER_QUEUE);
        self.state.lock().unwrap().listeners.push(sender);
        receiver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer() -> Mixer {
        let static_source =
            StaticSource::new(include_bytes!("../assets/static.mp3"), 44100).unwrap();
        Mixer::new(static_source)
    }

    #[test]
    fn output_is_silent_until_a_volume_is_raised() {
        let mixer = mixer();
        let mut out = vec![1.0; 4096];

        mixer.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));

        mixer.set_volumes(0.0, 1.0);
        mixer.fill(&mut out);
        assert!(out.iter().any(|s| *s != 0.0));
    }

    #[test]
    fn listeners_receive_a_copy_of_the_mix() {
        let mixer = mixer();
        mixer.set_volumes(0.0, 1.0);
        let listener = mixer.subscribe();
        let mut out = vec![0.0; 4096];

        mixer.fill(&mut out);

        let chunk = listener.try_recv().unwrap();
        assert_eq!(&*chunk, &out[..]);
    }

    #[test]
    fn a_departed_listener_is_forgotten() {
        let mixer = mixer();
        drop(mixer.subscribe());

        mixer.fill(&mut [0.0; 64]);

        assert!(mixer.state.lock().unwrap().listeners.is_empty());
    }
}
