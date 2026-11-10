//! Serves the mixed audio over HTTP as an endless MP3 stream at `/stream`, the way an
//! internet radio station does.
//!
//! MP3 rather than plain WAV because Apple's AVPlayer (and so any player built on it)
//! refuses a live WAV stream, but plays live MP3.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rusty_mp3::encode::Mp3Encode;
use rusty_mp3::header::FrameHeader;

use crate::mixer::Mixer;
use crate::source::{CHANNELS, Resampler};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CAST_KBPS: u32 = 192;
/// The sample rates MP3 can carry. Sound cards running at anything else are converted.
const MP3_RATES: [u32; 9] = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000];
const FALLBACK_RATE: u32 = 48000;

/// A running HTTP server. Dropping it stops the server and disconnects its listeners.
pub struct Server {
    url: String,
    local_address: SocketAddr,
    stopping: Arc<AtomicBool>,
    mixer: Arc<Mixer>,
}

pub fn start(address: &str, mixer: Arc<Mixer>, rate: u32) -> io::Result<Server> {
    let listener = TcpListener::bind(address)?;
    let local_address = reachable(listener.local_addr()?);
    let url = format!("http://{local_address}/stream");
    let stopping = Arc::new(AtomicBool::new(false));

    let (thread_mixer, thread_stopping) = (mixer.clone(), stopping.clone());
    thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            if thread_stopping.load(Ordering::Relaxed) {
                break;
            }
            let (mixer, stopping) = (thread_mixer.clone(), thread_stopping.clone());
            // A listener going away shows up as a write error, which just ends its thread.
            thread::spawn(move || handle(connection, &mixer, &stopping, rate));
        }
    });
    Ok(Server { url, local_address, stopping, mixer })
}

impl Server {
    /// The stream's address for players on this machine, which always works whatever
    /// networks or VPNs are connected. Other machines need this one's address instead.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.mixer.disconnect_listeners();
        // The accept loop only looks at the flag when a connection arrives, so make one.
        let _ = TcpStream::connect_timeout(&self.local_address, Duration::from_secs(1));
    }
}

/// An address we can connect to ourselves: "every interface" becomes this machine.
fn reachable(mut address: SocketAddr) -> SocketAddr {
    if address.ip().is_unspecified() {
        address.set_ip(match address {
            SocketAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    address
}

fn handle(
    mut connection: TcpStream,
    mixer: &Mixer,
    stopping: &AtomicBool,
    rate: u32,
) -> io::Result<()> {
    connection.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let mut lines = BufReader::new(connection.try_clone()?).lines();
    let request_line = lines.next().transpose()?.unwrap_or_default();
    // Read past the request headers so the connection is clean before we reply.
    for line in lines {
        if line?.is_empty() {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    if path != "/" && path != "/stream" {
        return connection.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n");
    }

    connection.write_all(
        b"HTTP/1.0 200 OK\r\n\
          Content-Type: audio/mpeg\r\n\
          Cache-Control: no-cache\r\n\
          Connection: close\r\n\r\n",
    )?;
    if stopping.load(Ordering::Relaxed) {
        return Ok(());
    }

    // Each listener gets its own encoder, so its stream starts cleanly at a frame boundary.
    let stream_rate = mp3_rate(rate);
    let mut resampler = Resampler::new(stream_rate);
    let mut encoder = LiveEncoder::new(stream_rate)?;
    let (mut resampled, mut mp3) = (Vec::new(), Vec::new());
    for chunk in mixer.subscribe() {
        resampler.process(rate, &chunk, &mut resampled);
        mp3.clear();
        encoder.encode(&resampled, &mut mp3);
        connection.write_all(&mp3)?;
    }
    Ok(())
}

/// The rate to encode at: the engine's own if MP3 can carry it.
fn mp3_rate(engine_rate: u32) -> u32 {
    if MP3_RATES.contains(&engine_rate) { engine_rate } else { FALLBACK_RATE }
}

/// Turns interleaved stereo into MP3 a frame at a time, holding nothing back, as a live
/// stream needs. (The crate's higher-level encoder saves its frames up until the end.)
struct LiveEncoder {
    header: FrameHeader,
    state: Mp3Encode,
    /// Audio waiting to fill the next frame, one buffer per channel.
    staged: [Vec<f32>; CHANNELS],
}

impl LiveEncoder {
    fn new(rate: u32) -> io::Result<Self> {
        let header = rusty_mp3::encoder_header(rate, CHANNELS as u16, CAST_KBPS)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self { header, state: Mp3Encode::new(), staged: [Vec::new(), Vec::new()] })
    }

    /// Takes in more audio and appends every frame it completes to `out`.
    fn encode(&mut self, interleaved: &[f32], out: &mut Vec<u8>) {
        for frame in interleaved.as_chunks::<CHANNELS>().0 {
            for (staged, sample) in self.staged.iter_mut().zip(frame) {
                staged.push(sample.clamp(-1.0, 1.0));
            }
        }
        let per_frame = self.header.version.samples_per_frame();
        while self.staged[0].len() >= per_frame {
            let block = [&self.staged[0][..per_frame], &self.staged[1][..per_frame]];
            // The crate documents that this cannot fail today. If that changes, drop the frame.
            if let Ok(frame) = self.state.encode_frame(&self.header, &block, None) {
                out.extend(frame);
            }
            for staged in &mut self.staged {
                staged.drain(..per_frame);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Instant;

    use super::*;
    use crate::source::StaticSource;

    fn mixer() -> Arc<Mixer> {
        let static_source =
            StaticSource::new(include_bytes!("../assets/static.mp3"), 44100).unwrap();
        Arc::new(Mixer::new(static_source))
    }

    fn request_stream(address: SocketAddr) -> io::Result<TcpStream> {
        let mut connection = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
        connection.set_read_timeout(Some(Duration::from_secs(2)))?;
        connection.write_all(b"GET /stream HTTP/1.0\r\n\r\n")?;
        Ok(connection)
    }

    #[test]
    fn the_encoder_gives_out_each_frame_as_soon_as_it_has_the_audio_for_it() {
        let mut encoder = LiveEncoder::new(48000).unwrap();
        let mut mp3 = Vec::new();

        // Just short of one frame: nothing yet.
        encoder.encode(&vec![0.1; 1151 * CHANNELS], &mut mp3);
        assert!(mp3.is_empty());

        // One more sample completes it. A 192 kbps frame at 48 kHz is 576 bytes.
        encoder.encode(&[0.1; CHANNELS], &mut mp3);
        assert_eq!(mp3.len(), 576);
        assert!(mp3[0] == 0xFF && mp3[1] & 0xE0 == 0xE0, "frame starts {:02x?}", &mp3[..4]);

        // A second's worth more gives 41 whole frames, with the rest kept for next time.
        mp3.clear();
        encoder.encode(&vec![0.1; 48000 * CHANNELS], &mut mp3);
        assert_eq!(mp3.len(), 41 * 576);
    }

    #[test]
    fn rates_mp3_cannot_carry_are_converted() {
        assert_eq!(mp3_rate(44100), 44100);
        assert_eq!(mp3_rate(48000), 48000);
        assert_eq!(mp3_rate(96000), FALLBACK_RATE);
    }

    #[test]
    fn a_listener_gets_an_mp3_stream_until_the_server_is_dropped() {
        let mixer = mixer();
        mixer.set_volumes(0.0, 1.0);
        let server = start("127.0.0.1:0", mixer.clone(), 44100).unwrap();
        let address = server.local_address;
        let mut connection = request_stream(address).unwrap();

        // Stand in for the sound card, pulling audio through the mixer.
        let playing = Arc::new(AtomicBool::new(true));
        let sound_card = thread::spawn({
            let (mixer, playing) = (mixer.clone(), playing.clone());
            move || {
                let mut out = vec![0.0; 2048];
                while playing.load(Ordering::Relaxed) {
                    mixer.fill(&mut out);
                    thread::sleep(Duration::from_millis(2));
                }
            }
        });
        let mut reply = vec![0u8; 4096];
        connection.read_exact(&mut reply).unwrap();
        playing.store(false, Ordering::Relaxed);
        sound_card.join().unwrap();

        let headers_end = reply.windows(4).position(|bytes| bytes == b"\r\n\r\n").unwrap() + 4;
        let headers = String::from_utf8_lossy(&reply[..headers_end]);
        assert!(headers.starts_with("HTTP/1.0 200 OK"), "got: {headers}");
        assert!(headers.contains("audio/mpeg"));
        // Every MP3 frame opens with eleven set bits.
        let body = &reply[headers_end..];
        assert!(body[0] == 0xFF && body[1] & 0xE0 == 0xE0, "body starts {:02x?}", &body[..4]);

        drop(server);

        // The listener is cut off, and soon nobody is answering on the port at all.
        let mut rest = Vec::new();
        connection.read_to_end(&mut rest).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
            assert!(Instant::now() < deadline, "server still accepting connections");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_url_names_the_address_it_was_bound_to() {
        let server = start("127.0.0.1:0", mixer(), 44100).unwrap();

        assert_eq!(server.url(), format!("http://{}/stream", server.local_address));
    }

    #[test]
    fn listening_on_every_interface_gives_this_machines_own_address() {
        let server = start("0.0.0.0:0", mixer(), 44100).unwrap();

        let port = server.local_address.port();
        assert_eq!(server.url(), format!("http://127.0.0.1:{port}/stream"));
    }

    #[test]
    fn other_paths_are_not_found() {
        let server = start("127.0.0.1:0", mixer(), 44100).unwrap();
        let mut connection = TcpStream::connect(server.local_address).unwrap();
        connection.write_all(b"GET /nope HTTP/1.0\r\n\r\n").unwrap();

        let mut reply = String::new();
        connection.read_to_string(&mut reply).unwrap();

        assert!(reply.starts_with("HTTP/1.0 404"), "got: {reply}");
    }
}
