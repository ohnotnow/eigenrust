mod mixer;
mod output;
mod player;
mod playlist;
mod radio;
mod server;
mod settings;
mod source;
mod stations;
mod ui;

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use mixer::Mixer;
use playlist::Station;
use radio::Timing;
use settings::Settings;
use source::{StaticSource, StreamSource};
use stations::StationManager;

const STATIC_MP3: &[u8] = include_bytes!("../assets/static.mp3");
const APP_FOLDER: &str = "EigenRadio";
const DEFAULT_SERVER_ADDRESS: &str = "0.0.0.0:8090";
const DEFAULT_STAY: Duration = Duration::from_secs(30);
const CHECK_TIMEOUT: Duration = Duration::from_secs(6);
const CHECK_RATE: u32 = 44100;
const CHECK_BATCH: usize = 16;

const USAGE: &str = "\
Usage: eigenrust [OPTIONS]

  --playlist PATH               Playlist file, .m3u or Icecast .xml
                                (default: the last one chosen, else the built-in list)
  --track-duration SECONDS      How long to stay on each station
                                (default: the knob's last setting, else 30)
  --crossfade-duration SECONDS  Length of the tune through static (default: 3)
  --server[=HOST:PORT]          Start with the HTTP stream on (default: 0.0.0.0:8090)
  --mute                        Start with the speakers silenced (useful with --server)
  --cli                         Run in the terminal with no window
  --data-dir PATH               Where to keep station history and settings
  --check                       Test every station in the playlist, report which play, and exit";

struct Args {
    playlist: Option<PathBuf>,
    track_duration: Option<Duration>,
    crossfade_duration: Duration,
    server: Option<String>,
    mute: bool,
    cli: bool,
    data_dir: Option<PathBuf>,
    check: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        playlist: None,
        track_duration: None,
        crossfade_duration: Duration::from_secs(3),
        server: None,
        mute: false,
        cli: false,
        data_dir: None,
        check: false,
    };

    let mut values = std::env::args().skip(1);
    while let Some(flag) = values.next() {
        let mut value = || values.next().ok_or(format!("{flag} needs a value"));
        let seconds = |text: String| {
            text.parse().map(Duration::from_secs_f64).map_err(|_| format!("bad number: {text}"))
        };
        match flag.as_str() {
            "--playlist" => args.playlist = Some(PathBuf::from(value()?)),
            "--track-duration" => args.track_duration = Some(seconds(value()?)?),
            "--crossfade-duration" => args.crossfade_duration = seconds(value()?)?,
            "--server" => args.server = Some(DEFAULT_SERVER_ADDRESS.to_string()),
            "--mute" => args.mute = true,
            "--cli" => args.cli = true,
            "--data-dir" => args.data_dir = Some(PathBuf::from(value()?)),
            "--check" => args.check = true,
            _ => match flag.strip_prefix("--server=") {
                Some(address) => args.server = Some(address.to_string()),
                None => return Err(format!("unknown option: {flag}")),
            },
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args().map_err(|e| format!("{e}\n\n{USAGE}"))?;

    let data_dir = match args.data_dir {
        Some(path) => path,
        None => dirs::data_dir().ok_or("cannot find a folder for app data")?.join(APP_FOLDER),
    };
    fs::create_dir_all(&data_dir)?;
    let settings_path = data_dir.join("settings.json");
    let settings: Settings = settings::load_or_default(&settings_path)?;

    let stations = match (&args.playlist, &settings.playlist) {
        (Some(path), _) => playlist::load_playlist(path)?,
        // A remembered playlist may have been moved or deleted since. Carry on without it.
        (None, Some(path)) => playlist::load_playlist(path).unwrap_or_else(|e| {
            eprintln!("Using the built-in stations: {e}");
            playlist::built_in()
        }),
        (None, None) => playlist::built_in(),
    };
    println!("Loaded {} stations.", stations.len());
    if args.check {
        check_stations(&stations);
        return Ok(());
    }
    let manager = StationManager::load(data_dir.join("station_data.json"))?;

    let (device, config) = output::default_device()?;
    let rate = config.sample_rate();
    let mixer = Arc::new(Mixer::new(StaticSource::new(STATIC_MP3, rate)?));
    let muted = Arc::new(AtomicBool::new(args.mute));
    let _speakers = output::start(&device, config, mixer.clone(), muted.clone())?;

    let server = match &args.server {
        Some(address) => {
            println!("HTTP audio server listening on http://{address}/stream");
            Some(server::start(address, mixer.clone(), rate)?)
        }
        None => None,
    };
    let stay = args
        .track_duration
        .or(settings.stay_seconds.map(Duration::from_secs))
        .unwrap_or(DEFAULT_STAY);
    let timing = Timing { track: stay, crossfade: args.crossfade_duration };

    if args.cli {
        // The radio logs to the terminal as it goes, so there is nothing to do with its events.
        let (_commands, radio) = radio::start(mixer, rate, stations, manager, timing, |_| {});
        radio.join().map_err(|_| "the radio stopped unexpectedly")?;
    } else {
        let (to_face, events) = futures::channel::mpsc::unbounded();
        let (commands, _radio) =
            radio::start(mixer.clone(), rate, stations, manager, timing, move |event| {
                // The window closing ends the whole app, so a failed send needs no handling.
                let _ = to_face.unbounded_send(event);
            });
        ui::run(ui::Setup {
            mixer,
            rate,
            muted,
            commands,
            events,
            server,
            server_address: args.server.unwrap_or_else(|| DEFAULT_SERVER_ADDRESS.to_string()),
            stay,
            settings,
            settings_path,
        });
    }
    Ok(())
}

/// Tries every station, a batch at a time, printing whether each one produced audio.
fn check_stations(stations: &[Station]) {
    for batch in stations.chunks(CHECK_BATCH) {
        thread::scope(|scope| {
            for station in batch {
                scope.spawn(|| {
                    let stream = StreamSource::open(&station.url, CHECK_RATE);
                    let verdict = if stream.wait_for_audio(CHECK_TIMEOUT) { "OK" } else { "FAIL" };
                    println!("{verdict}\t{}", station.url);
                });
            }
        });
    }
}
