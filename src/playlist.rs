use std::fs;
use std::path::Path;

const UNKNOWN_NAME: &str = "Unknown Station";

#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    pub url: String,
    pub name: String,
    pub genre: Option<String>,
}

impl Station {
    /// The name from the playlist, if it gave one.
    pub fn listed_name(&self) -> Option<&str> {
        (self.name != UNKNOWN_NAME).then_some(self.name.as_str())
    }

    /// A fixed place on the dial for this station, from 0 to 1, worked out from its address.
    ///
    /// Uses the FNV-1a hash because, unlike the standard library's hasher, its results never
    /// change between Rust versions, so stations stay put across upgrades.
    pub fn dial_position(&self) -> f32 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.url.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        (hash >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// The stations that ship inside the app, used until the user chooses a playlist.
pub fn built_in() -> Vec<Station> {
    parse_m3u(include_str!("../example.m3u"))
}

/// Loads stations from a playlist file (`.m3u` or Icecast `.xml`).
pub fn load_playlist(path: &Path) -> Result<Vec<Station>, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match extension.to_ascii_lowercase().as_str() {
        "xml" => parse_icecast_xml(&text),
        "m3u" => Ok(parse_m3u(&text)),
        other => Err(format!("unsupported playlist format: .{other}")),
    }
}

fn parse_icecast_xml(text: &str) -> Result<Vec<Station>, String> {
    let document = roxmltree::Document::parse(text).map_err(|e| format!("invalid XML: {e}"))?;
    let stations = document
        .descendants()
        .filter(|node| node.has_tag_name("entry"))
        .filter_map(|entry| {
            let field = |tag: &str| {
                entry
                    .children()
                    .find(|child| child.has_tag_name(tag))
                    .and_then(|child| child.text())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            Some(Station {
                url: field("listen_url")?,
                name: field("server_name").unwrap_or_else(|| UNKNOWN_NAME.to_string()),
                genre: field("genre"),
            })
        })
        .collect();
    Ok(stations)
}

fn parse_m3u(text: &str) -> Vec<Station> {
    let mut stations = Vec::new();
    let mut name = UNKNOWN_NAME.to_string();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("#EXTINF") {
            // Format: #EXTINF:123,Station Name
            if let Some((_, title)) = line.split_once(',') {
                name = title.trim().to_string();
            }
        } else if !line.starts_with('#') {
            stations.push(Station {
                url: line.to_string(),
                name: std::mem::replace(&mut name, UNKNOWN_NAME.to_string()),
                genre: None,
            });
        }
    }
    stations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_pairs_names_with_urls() {
        let stations = parse_m3u(
            "#EXTM3U\n#EXTINF:-1,Jazz FM\nhttp://example.com/jazz\n\nhttp://example.com/unnamed\n",
        );

        assert_eq!(stations.len(), 2);
        assert_eq!(stations[0].name, "Jazz FM");
        assert_eq!(stations[0].url, "http://example.com/jazz");
        assert_eq!(stations[1].name, UNKNOWN_NAME);
        assert_eq!(stations[1].url, "http://example.com/unnamed");
    }

    #[test]
    fn icecast_xml_reads_entries_and_skips_those_without_a_url() {
        let stations = parse_icecast_xml(
            "<directory>\
             <entry><server_name>Rock One</server_name><listen_url> http://example.com/rock </listen_url><genre>Rock</genre></entry>\
             <entry><listen_url>http://example.com/bare</listen_url></entry>\
             <entry><server_name>No URL</server_name></entry>\
             </directory>",
        )
        .unwrap();

        assert_eq!(stations.len(), 2);
        assert_eq!(stations[0].name, "Rock One");
        assert_eq!(stations[0].url, "http://example.com/rock");
        assert_eq!(stations[0].genre.as_deref(), Some("Rock"));
        assert_eq!(stations[1].name, UNKNOWN_NAME);
        assert_eq!(stations[1].genre, None);
    }

    #[test]
    fn a_station_always_sits_at_the_same_place_on_the_dial() {
        let station = |url: &str| Station { url: url.to_string(), name: String::new(), genre: None };

        // Known FNV-1a value, so a change of hash would show up here.
        assert_eq!(station("a").dial_position(), 0xaf63dc as f32 / (1u64 << 24) as f32);
        assert_ne!(
            station("http://example.com/jazz").dial_position(),
            station("http://example.com/rock").dial_position()
        );
        for url in ["", "a", "http://example.com/a-much-longer-address/stream.mp3"] {
            assert!((0.0..1.0).contains(&station(url).dial_position()));
        }
    }

    #[test]
    fn the_built_in_list_has_stations() {
        assert!(built_in().len() > 10);
    }

    #[test]
    fn broken_xml_is_an_error() {
        assert!(parse_icecast_xml("<directory><entry>").is_err());
    }
}
