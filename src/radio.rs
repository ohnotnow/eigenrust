//! The radio itself: picks stations, tunes between them, and takes orders from the controls.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::mixer::Mixer;
use crate::player;
use crate::playlist::Station;
use crate::source::StreamSource;
use crate::stations::StationManager;

const PRESAMPLE_TIMEOUT: Duration = Duration::from_secs(6);
const OFFLINE_PAUSE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(250);

/// An order from the controls.
pub enum Command {
    Retune,
    SetPaused(bool),
    /// Change how long each station plays. Applies to the current station too.
    SetStay(Duration),
    Playlist(Vec<Station>),
}

/// Something for the radio's face to show.
pub enum Event {
    /// Tuning has begun: send the needle to this place on the dial (0 to 1).
    Spin(f32),
    /// Text for the station window.
    Show(String),
}

pub struct Timing {
    pub track: Duration,
    pub crossfade: Duration,
}

/// Starts the radio on its own thread. It runs until the returned sender is dropped,
/// or it runs out of stations with nobody left to give it more.
pub fn start(
    mixer: Arc<Mixer>,
    rate: u32,
    stations: Vec<Station>,
    manager: StationManager,
    timing: Timing,
    notify: impl Fn(Event) + Send + 'static,
) -> (Sender<Command>, JoinHandle<()>) {
    let (sender, commands) = mpsc::channel();
    let radio = Radio { mixer, rate, stations, manager, timing, notify, commands, paused: false };
    (sender, thread::spawn(move || radio.run()))
}

struct Radio<N> {
    mixer: Arc<Mixer>,
    rate: u32,
    stations: Vec<Station>,
    manager: StationManager,
    timing: Timing,
    notify: N,
    commands: Receiver<Command>,
    paused: bool,
}

impl<N: Fn(Event)> Radio<N> {
    fn run(mut self) {
        loop {
            // Orders that piled up while we were busy collapse into where things stand now.
            while let Ok(command) = self.commands.try_recv() {
                self.obey(command);
            }

            if self.paused {
                if self.mixer.has_stream() {
                    player::fade_out(&self.mixer);
                }
                self.show("Paused");
                if !self.wait_for_order() {
                    return;
                }
                continue;
            }

            if !self.mixer.has_stream() && !self.manager.seems_offline() {
                self.show("Finding a station...");
            }
            let Some(station) = self.manager.pick_next(&self.stations).cloned() else {
                println!("No stations available: all are resting after failures, or banned.");
                self.show("No stations left to try");
                if !self.wait_for_order() {
                    return;
                }
                continue;
            };
            println!("\nCandidate: {} ({})", station.name, station.genre.as_deref().unwrap_or("Unknown"));
            println!("URL: {}", station.url);

            // The current station keeps playing while the candidate is checked for real audio.
            let stream = StreamSource::open(&station.url, self.rate);
            if !stream.wait_for_audio(PRESAMPLE_TIMEOUT) {
                println!("Station failed validation.");
                self.manager.record_failure(&station.url);
                if self.manager.seems_offline() {
                    println!("Several failures in a row, the network may be down. Pausing...");
                    self.show("No signal. Retrying...");
                    let retry_at = Instant::now() + OFFLINE_PAUSE;
                    if !self.wait(|_| retry_at, false) {
                        return;
                    }
                }
                continue;
            }

            if let Err(e) = self.manager.record_play(&station.url) {
                eprintln!("Could not save station data: {e}");
            }
            // Prefer the playlist's name, then the station's own, then admit we don't know.
            let name = match station.listed_name() {
                Some(listed) => listed.to_string(),
                None => stream.name().unwrap_or_else(|| station.name.clone()),
            };
            println!("Tuning in to {name}...");
            (self.notify)(Event::Spin(station.dial_position()));
            player::tune(&self.mixer, stream, self.timing.crossfade, || {
                (self.notify)(Event::Show(name));
            });

            println!("Playing for {} seconds...", self.timing.track.as_secs_f64());
            let started = Instant::now();
            if !self.wait(|radio| started + radio.timing.track, true) {
                return;
            }
        }
    }

    /// Waits for a reason to move on: the deadline passing, an order, or (while `listening`)
    /// the station going dead. The deadline is asked for afresh each time round, so an order
    /// can move it while we wait. Returns false if the controls have gone away.
    fn wait(&mut self, deadline: impl Fn(&Self) -> Instant, listening: bool) -> bool {
        loop {
            let remaining = deadline(self).saturating_duration_since(Instant::now());
            if remaining.is_zero() || (listening && self.mixer.stream_finished()) {
                return true;
            }
            match self.commands.recv_timeout(remaining.min(POLL)) {
                Ok(command) => {
                    if self.obey(command) {
                        return true;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Waits as long as it takes for the next order. Returns false if the controls have gone away.
    fn wait_for_order(&mut self) -> bool {
        match self.commands.recv() {
            Ok(command) => {
                self.obey(command);
                true
            }
            Err(_) => false,
        }
    }

    /// Applies an order. Returns true if it means dropping what we're doing and moving on.
    fn obey(&mut self, command: Command) -> bool {
        match command {
            Command::Retune => {
                self.paused = false;
                true
            }
            Command::SetPaused(paused) => {
                let changed = paused != self.paused;
                self.paused = paused;
                changed
            }
            Command::SetStay(track) => {
                self.timing.track = track;
                false
            }
            Command::Playlist(stations) => {
                self.stations = stations;
                self.paused = false;
                true
            }
        }
    }

    fn show(&self, text: &str) {
        (self.notify)(Event::Show(text.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::StaticSource;

    const QUICKLY: Duration = Duration::from_secs(2);

    fn radio(track: Duration) -> (Sender<Command>, Radio<impl Fn(Event)>) {
        let static_source =
            StaticSource::new(include_bytes!("../assets/static.mp3"), 44100).unwrap();
        let (sender, commands) = mpsc::channel();
        let radio = Radio {
            mixer: Arc::new(Mixer::new(static_source)),
            rate: 44100,
            stations: Vec::new(),
            manager: StationManager::load(std::env::temp_dir().join("eigenrust-no-such-data.json"))
                .unwrap(),
            timing: Timing { track, crossfade: Duration::ZERO },
            notify: |_| {},
            commands,
            paused: false,
        };
        (sender, radio)
    }

    #[test]
    fn shortening_the_stay_cuts_short_the_station_that_is_playing() {
        let (orders, mut radio) = radio(Duration::from_secs(3600));
        orders.send(Command::SetStay(Duration::from_millis(50))).unwrap();

        let started = Instant::now();
        let controls_present = radio.wait(|radio| started + radio.timing.track, false);

        assert!(controls_present);
        assert!(started.elapsed() < QUICKLY);
        assert_eq!(radio.timing.track, Duration::from_millis(50));
    }

    #[test]
    fn lengthening_the_stay_keeps_the_station_that_is_playing() {
        let (orders, mut radio) = radio(Duration::from_millis(300));
        orders.send(Command::SetStay(Duration::from_millis(900))).unwrap();

        let started = Instant::now();
        radio.wait(|radio| started + radio.timing.track, false);

        assert!(started.elapsed() >= Duration::from_millis(900));
    }

    #[test]
    fn a_retune_order_ends_the_wait_at_once() {
        let (orders, mut radio) = radio(Duration::from_secs(3600));
        orders.send(Command::Retune).unwrap();

        let started = Instant::now();
        radio.wait(|radio| started + radio.timing.track, false);

        assert!(started.elapsed() < QUICKLY);
    }

    #[test]
    fn the_wait_reports_when_the_controls_have_gone() {
        let (orders, mut radio) = radio(Duration::from_secs(3600));
        drop(orders);

        let started = Instant::now();
        assert!(!radio.wait(|radio| started + radio.timing.track, false));
    }

    #[test]
    fn only_a_change_of_pause_state_interrupts() {
        let (_orders, mut radio) = radio(Duration::from_secs(30));

        assert!(radio.obey(Command::SetPaused(true)));
        assert!(!radio.obey(Command::SetPaused(true)));
        assert!(radio.obey(Command::Retune));
        assert!(!radio.paused, "retuning should also resume");
    }
}
