use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::playlist::Station;
use crate::settings;

const HISTORY_LIMIT: usize = 100;
const RECENT_COUNT: usize = 20;
/// How long a station is left alone after a failure before it gets another go.
const REST_SECONDS: u64 = 3 * 24 * 60 * 60;
/// Failures (with no successful play in between) before a station is dropped for good.
const STRIKES_FOR_BAN: u32 = 3;
/// This many failures in a row looks like our network being down, not the stations.
const OFFLINE_RUN: usize = 5;

#[derive(Serialize, Deserialize)]
struct Failure {
    count: u32,
    last_failed: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct StationData {
    #[serde(default)]
    failures: BTreeMap<String, Failure>,
    #[serde(default)]
    history: Vec<String>,
}

/// Remembers which stations failed and which were played recently.
///
/// A failure only counts once a later station plays, which proves the network was up.
/// Until then it is held in `pending`.
pub struct StationManager {
    path: PathBuf,
    data: StationData,
    pending: Vec<String>,
    failures_in_a_row: usize,
}

impl StationManager {
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let data = settings::load_or_default(&path)?;
        Ok(Self { path, data, pending: Vec::new(), failures_in_a_row: 0 })
    }

    pub fn record_failure(&mut self, url: &str) {
        self.failures_in_a_row += 1;
        if self.seems_offline() {
            self.pending.clear();
        } else {
            self.pending.push(url.to_string());
        }
    }

    /// True after enough failures in a row that the network itself is the likely culprit.
    pub fn seems_offline(&self) -> bool {
        self.failures_in_a_row >= OFFLINE_RUN
    }

    pub fn record_play(&mut self, url: &str) -> io::Result<()> {
        self.played(url, now());
        self.save()
    }

    /// Picks a random station that is not resting or banned, preferring ones not played recently.
    pub fn pick_next<'a>(&self, stations: &'a [Station]) -> Option<&'a Station> {
        self.pick_next_at(stations, now())
    }

    fn played(&mut self, url: &str, now: u64) {
        self.failures_in_a_row = 0;
        for failed in std::mem::take(&mut self.pending) {
            let failure =
                self.data.failures.entry(failed).or_insert(Failure { count: 0, last_failed: 0 });
            failure.count += 1;
            failure.last_failed = now;
        }
        self.data.failures.remove(url);

        self.data.history.push(url.to_string());
        if self.data.history.len() > HISTORY_LIMIT {
            self.data.history.remove(0);
        }
    }

    fn pick_next_at<'a>(&self, stations: &'a [Station], now: u64) -> Option<&'a Station> {
        let valid: Vec<&Station> =
            stations.iter().filter(|s| self.is_available(&s.url, now)).collect();
        let recent: HashSet<&str> =
            self.data.history.iter().rev().take(RECENT_COUNT).map(String::as_str).collect();
        let fresh: Vec<&Station> =
            valid.iter().copied().filter(|s| !recent.contains(s.url.as_str())).collect();

        let pool = if fresh.is_empty() { valid } else { fresh };
        fastrand::choice(pool)
    }

    fn is_available(&self, url: &str, now: u64) -> bool {
        if self.pending.iter().any(|pending| pending == url) {
            return false;
        }
        match self.data.failures.get(url) {
            Some(failure) => {
                failure.count < STRIKES_FOR_BAN
                    && now.saturating_sub(failure.last_failed) >= REST_SECONDS
            }
            None => true,
        }
    }

    fn save(&self) -> io::Result<()> {
        settings::save(&self.path, &self.data)
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;

    fn station(url: &str) -> Station {
        Station { url: url.to_string(), name: url.to_string(), genre: None }
    }

    fn manager() -> StationManager {
        StationManager {
            path: PathBuf::new(),
            data: StationData::default(),
            pending: Vec::new(),
            failures_in_a_row: 0,
        }
    }

    /// One confirmed failure for `url`: it fails, then another station plays.
    fn fail(manager: &mut StationManager, url: &str, at: u64) {
        manager.record_failure(url);
        manager.played("other", at);
    }

    fn picks(manager: &StationManager, stations: &[Station], at: u64) -> HashSet<String> {
        (0..100).filter_map(|_| manager.pick_next_at(stations, at)).map(|s| s.url.clone()).collect()
    }

    #[test]
    fn a_failed_station_is_not_retried_straight_away() {
        let stations = [station("flaky"), station("good")];
        let mut manager = manager();

        manager.record_failure("flaky");

        assert_eq!(picks(&manager, &stations, NOW), HashSet::from(["good".to_string()]));
    }

    #[test]
    fn a_failed_station_rests_then_comes_back() {
        let stations = [station("flaky")];
        let mut manager = manager();

        fail(&mut manager, "flaky", NOW);

        assert!(manager.pick_next_at(&stations, NOW + REST_SECONDS - 1).is_none());
        assert_eq!(manager.pick_next_at(&stations, NOW + REST_SECONDS).unwrap().url, "flaky");
    }

    #[test]
    fn a_repeat_offender_is_banned_for_good() {
        let stations = [station("dead")];
        let mut manager = manager();

        for strike in 0..STRIKES_FOR_BAN as u64 {
            fail(&mut manager, "dead", NOW + strike * REST_SECONDS);
        }

        assert!(manager.pick_next_at(&stations, NOW + 1000 * REST_SECONDS).is_none());
    }

    #[test]
    fn a_successful_play_wipes_a_stations_strikes() {
        let mut manager = manager();
        fail(&mut manager, "flaky", NOW);
        fail(&mut manager, "flaky", NOW + REST_SECONDS);

        manager.played("flaky", NOW + 2 * REST_SECONDS);

        assert!(!manager.data.failures.contains_key("flaky"));
    }

    #[test]
    fn a_run_of_failures_is_treated_as_being_offline_and_forgotten() {
        let stations: Vec<Station> = (0..OFFLINE_RUN).map(|i| station(&format!("s{i}"))).collect();
        let mut manager = manager();

        for station in &stations {
            manager.record_failure(&station.url);
        }
        assert!(manager.seems_offline());

        // Back online: nothing that failed during the outage is held against it.
        manager.played("other", NOW);
        assert!(!manager.seems_offline());
        assert!(manager.data.failures.is_empty());
        assert_eq!(picks(&manager, &stations, NOW).len(), OFFLINE_RUN);
    }

    #[test]
    fn prefers_stations_not_played_recently() {
        let stations = [station("recent"), station("fresh")];
        let mut manager = manager();
        manager.played("recent", NOW);

        assert_eq!(picks(&manager, &stations, NOW), HashSet::from(["fresh".to_string()]));
    }

    #[test]
    fn falls_back_to_recent_stations_when_nothing_is_fresh() {
        let stations = [station("recent")];
        let mut manager = manager();
        manager.played("recent", NOW);

        assert_eq!(manager.pick_next_at(&stations, NOW).unwrap().url, "recent");
    }
}
