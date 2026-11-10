//! Small JSON files kept in the user's data folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// What the app remembers between runs.
#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    /// The playlist the user last chose, if any. Without one the built-in list is used.
    #[serde(default)]
    pub playlist: Option<PathBuf>,
    /// How long each station plays, as last set on the knob.
    #[serde(default)]
    pub stay_seconds: Option<u64>,
}

/// Reads a JSON file, or gives the default if there is no file yet.
pub fn load_or_default<T: Default + DeserializeOwned>(path: &Path) -> io::Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_save_and_load() {
        let path = std::env::temp_dir().join(format!("eigenrust-settings-{}.json", fastrand::u64(..)));
        let settings =
            Settings { playlist: Some(PathBuf::from("/music/favourites.m3u")), stay_seconds: Some(300) };

        save(&path, &settings).unwrap();
        let loaded: Settings = load_or_default(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(loaded.playlist, settings.playlist);
        assert_eq!(loaded.stay_seconds, Some(300));
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let path = std::env::temp_dir().join("eigenrust-no-such-settings.json");

        let loaded: Settings = load_or_default(&path).unwrap();

        assert_eq!(loaded.playlist, None);
    }
}
