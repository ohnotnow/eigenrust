# Technical overview

Last updated: 2026-11-10

## What this is

A desktop internet radio that looks like a 1960s car radio. It hops between stations at random, tuning through static on the way. It is a Rust rewrite of the Python [EigenRadio 2](https://github.com/ohnotnow/eigen2), built so that nothing else needs installing.

Built and tested on Apple Silicon Macs only. Nothing has been compiled on Windows or Linux yet.

## Stack

- Rust 1.99, edition 2024
- gpui 0.2.2: the window (Zed's UI framework, the official crate from crates.io)
- symphonia 0.6.1: decoding MP3, AAC, Vorbis and FLAC streams
- cpal 0.18: audio output
- rusty_mp3 1.0.5: encoding the cast stream
- ureq 3.5: HTTP client for stations
- roxmltree, serde_json, dirs, fastrand, futures: small supporting crates

There is no async runtime and no FFmpeg. Everything outside the window runs on plain threads.

## Directory structure

```
src/
  main.rs       Argument parsing, start-up wiring, the --check station tester
  radio.rs      The radio loop: pick, test, tune, wait. Takes Commands, emits Events
  stations.rs   Which stations to avoid: strikes, resting, recent history
  playlist.rs   .m3u and Icecast .xml parsing, the built-in list, dial positions
  source.rs     StreamSource (a live station), StaticSource, decoding, resampling
  mixer.rs      Mixes station and static, hands copies to cast listeners
  player.rs     The fade sequence between stations
  output.rs     Speakers, via cpal
  server.rs     The cast stream: HTTP server and live MP3 encoder
  settings.rs   JSON load and save helpers, the Settings struct
  ui.rs         The GPUI window: dial, needle, keys, knobs
assets/         static.mp3 (compiled into the binary), EigenRadio.icns
example.m3u     The built-in station list, compiled into the binary
scripts/        lock_ages.py, package-macos.sh, make-icon.sh, icon.swift
.github/workflows/release.yml   Builds and publishes a release on a version tag
```

## How the pieces fit

```
 station ──HTTP──▶ StreamSource thread ──▶ 2 s buffer ─┐
                   (ureq + symphonia + resample)       │
                                                       ▼
 static.mp3 (decoded once at start) ───────────▶  Mixer::fill  ◀── called by the
                                                   │      │         sound card's
                                   speakers ◀──────┘      │         callback (cpal)
                                                          ▼
                                    one thread per cast listener:
                                    resample if needed ▶ LiveEncoder ▶ HTTP socket

 radio thread ──Event (Spin, Show)──▶ window          window ──Command──▶ radio thread
```

- The sound card's clock drives everything. `Mixer::fill` is called from cpal's callback, even when the speakers are muted, so cast listeners keep getting audio.
- The engine runs at whatever sample rate the default output device uses. Stations and the static are resampled to it (plain linear interpolation, `Resampler` in `source.rs`).
- The radio thread knows nothing about the window. It reports through a callback, which is how `--cli` runs the same loop with no window at all.
- The window receives events over a `futures` channel read by a task started with `cx.spawn`, and sends `Command`s back over a standard channel.

### The radio loop (`radio.rs`)

1. Ask `StationManager::pick_next` for a random station that is not resting or banned.
2. Open it and wait up to 6 seconds for audio louder than silence. The current station keeps playing meanwhile.
3. On failure, record it and try another. On success, send `Event::Spin`, run `player::tune`, and send `Event::Show` with the station's name at the moment the stream is swapped under the static.
4. Wait for the stay time, an order from the window, or the station going dead.

Orders that arrive while it is busy are drained and collapsed, so repeated presses of TUNE give one hop.

### Commands and events

| Message | Meaning |
|---|---|
| `Command::Retune` | Hop now |
| `Command::SetPaused(bool)` | Pause fades out and drops the station. Resume finds a new one |
| `Command::SetStay(Duration)` | New stay time. Applies to the station already playing |
| `Command::Playlist(Vec<Station>)` | Replace the station list and hop |
| `Event::Spin(f32)` | Tuning has begun: send the needle to this place on the dial |
| `Event::Show(String)` | Text for the station window |

## The cast stream

One route: a request for `/` or `/stream` returns an endless MP3 stream (192 kbps, `audio/mpeg`). Anything else is a 404. The server listens on `0.0.0.0:8090` by default, and the address offered for copying is always `http://127.0.0.1:8090/stream`.

## Files it keeps

In the per-user data folder (`~/Library/Application Support/EigenRadio` on a Mac, or `--data-dir`):

- `settings.json`: `playlist` (last chosen file) and `stay_seconds`
- `station_data.json`: `failures` (address to `count` and `last_failed`) and `history` (the last 100 addresses played)

## Key logic

| Location | Purpose |
|---|---|
| `stations.rs` `StationManager` | A failed station rests 3 days. 3 failures with no play in between bans it. A failure only counts once a later station plays. 5 failures in a row are treated as being offline and forgotten |
| `playlist.rs` `Station::dial_position` | FNV-1a hash of the address, so a station always sits at the same place on the dial |
| `radio.rs` `Radio::wait` | Asks for its deadline afresh each pass, which is how a new stay time takes effect mid-station |
| `player.rs` `tune` | Fade to static, hold half a second, swap, fade in |
| `source.rs` `receive` | Connects to a station. Also reads its `icy-name` header, used when the playlist gives no name |
| `server.rs` `LiveEncoder` | Frame-by-frame MP3 encoding for a live stream |
| `ui.rs` `needle_position` | The needle's path: one or two fast swings, then homing in with an overshoot |
| `ui.rs` `turn_stay`, `later` | The stay knob. The radio is told only after the knob has been left alone for 1.5 seconds |

## Things that will bite you

- `rusty_mp3::Mp3Encoder`, that crate's high-level encoder, gives out no data until it is finished when asked for constant bitrate at 256 kbps or less. `LiveEncoder` uses the lower-level `encode::Mp3Encode::encode_frame` for that reason.
- The cast stream is MP3 because Apple's AVPlayer refuses a live WAV or FLAC stream. It plays live MP3 and AAC.
- Do not guess the machine's network address from the default route. With a VPN connected that picks the VPN's address. This was tried and removed.
- GPUI can only rotate SVG elements, and only clips to rectangles. Hence the sliding needle, the dot markers on the knobs, and the two-part knob built from rectangular windows onto a full disc.
- GPUI gives an app no menu and no quit shortcut by default. Both are set up in `ui::run`.
- A double-clicked app has no useful working directory. Nothing may be read or written relative to it.
- GPUI's build needs Apple's Metal toolchain, which newer Xcodes download separately. The `runtime-shaders` feature avoids that.
- Old SHOUTcast servers that answer `ICY 200 OK` are rejected by ureq. Station addresses that point at a playlist file are not followed.
- HE-AAC stations decode at half bandwidth, and in mono for the v2 variant. Opus is not supported.

## Testing

- Framework: Rust's built-in test harness, tests beside the code in each file (40 tests)
- Pattern: pure logic is tested directly, with time passed in where it matters (`stations.rs`, `radio.rs`). The server tests use real sockets on a loopback port. Nothing tests the window itself
- Run: `cargo test` and `cargo clippy --all-targets`
- Checking stations against the real network: `cargo run --release -- --playlist FILE --check`

## Build options

- `runtime-shaders`: compile the Mac graphics shaders when the app starts instead of when it is built

## Local development

```bash
cargo run --release --features runtime-shaders        # the window
cargo run --release --features runtime-shaders -- --cli   # terminal only
python3 scripts/lock_ages.py      # after any change to Cargo.lock, before building
scripts/package-macos.sh          # dist/EigenRadio.app and a zip (needs the Metal toolchain)
```

`Cargo.lock` has several crates pinned back by hand so that nothing locked was less than a week old when it was added. A plain `cargo update` undoes those pins, so run `lock_ages.py` afterwards.

A release is made by pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml`.
