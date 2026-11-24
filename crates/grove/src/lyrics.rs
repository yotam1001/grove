//! Synced lyrics (#12) from LRCLIB: free, no key, and never Spotify's private
//! lyrics endpoint. Lookups use only public metadata (title, artist, album and
//! duration) and run while Now Playing is open. Results are cached per track.
//!
//! The network fetch is injectable: review modes pass [`fixture`], which never
//! touches the network.
use crate::{GroveLyricLine, GroveWindow};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::Read,
    rc::Rc,
    sync::{mpsc, Arc},
    time::Duration,
};

const MAX_LINES: usize = 1000;
const MAX_TEXT_BYTES: usize = 128_000;
const MAX_RESPONSE_BYTES: u64 = 256_000;
const CACHE_TRACKS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub start_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub synced: bool,
}

/// Public metadata for one recording. `artist` is the first credited artist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_s: u32,
}

/// `Ok(None)` means the service has no lyrics for this song.
pub type Fetcher = Arc<dyn Fn(&Query) -> Result<Option<Lyrics>, String> + Send + Sync>;

/// The real LRCLIB lookup: `/api/get` by title, artist, album and duration,
/// then `/api/search` by title and artist, accepting only close matches.
pub fn lrclib() -> Fetcher {
    Arc::new(|query: &Query| {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!(
                "Grove/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/yotam1001/grove)"
            ))
            .build()
            .map_err(|_| "Lyrics are unavailable right now.".to_string())?;
        let lookup = |endpoint: &str, params: &[(&str, String)]| -> Result<Option<Value>, String> {
            let url = url::Url::parse_with_params(
                &format!("https://lrclib.net/api/{endpoint}"),
                params.iter().map(|(k, v)| (*k, v.as_str())),
            )
            .map_err(|_| "Lyrics are unavailable right now.".to_string())?;
            let response = http
                .get(url)
                .send()
                .map_err(|_| "Couldn't reach LRCLIB.".to_string())?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err("LRCLIB is busy. Try again shortly.".into());
            }
            let mut bytes = Vec::new();
            response
                .take(MAX_RESPONSE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Couldn't read lyrics.".to_string())?;
            if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                return Ok(None);
            }
            Ok(serde_json::from_slice(&bytes).ok())
        };
        let params = vec![
            ("track_name", query.title.clone()),
            ("artist_name", query.artist.clone()),
        ];
        if query.duration_s > 0 && !query.album.is_empty() {
            let mut exact = params.clone();
            exact.push(("album_name", query.album.clone()));
            exact.push(("duration", query.duration_s.to_string()));
            if let Some(found) = lookup("get", &exact)?.and_then(|c| matched(&c, query)) {
                return Ok(Some(found));
            }
        }
        if let Some(Value::Array(candidates)) = lookup("search", &params)? {
            if let Some(found) = candidates.iter().take(20).find_map(|c| matched(c, query)) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    })
}

/// Offline lyrics for review modes. Titles containing "no lyrics" have none;
/// everything else gets synthetic synced lines. Never contacts the network.
pub fn fixture() -> Fetcher {
    Arc::new(|query: &Query| {
        if query.title.to_lowercase().contains("no lyrics") {
            return Ok(None);
        }
        let raw = "[00:00.00]\n[00:05.00]Morning light on the forest floor\n[00:12.50]Every branch a quiet chord\n[00:20.00]We follow roots where the rivers go\n[00:28.00]Grove of echoes, soft and slow\n[00:36.00]\n[00:40.00]Hold the record, let it spin\n[00:47.00]Every ring a place we've been\n[00:54.00]שיר של יער · أغنية الغابة\n[01:02.00]Green on charcoal, end to end";
        Ok(parse_lrc(raw, query.duration_s).map(|lines| Lyrics {
            lines,
            synced: true,
        }))
    })
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Spotify titles often carry " - Remastered 2011" or "(feat. …)".
fn base_title(value: &str) -> String {
    let cut = value
        .find(" - ")
        .into_iter()
        .chain(value.find(" ("))
        .chain(value.find(" ["))
        .min()
        .unwrap_or(value.len());
    normalize(&value[..cut])
}

fn matched(candidate: &Value, query: &Query) -> Option<Lyrics> {
    let title = candidate["trackName"].as_str()?;
    let artist = candidate["artistName"].as_str()?;
    let wanted_artist = normalize(&query.artist);
    if wanted_artist.is_empty()
        || (normalize(title) != normalize(&query.title)
            && base_title(title) != base_title(&query.title))
        || !normalize(artist).contains(&wanted_artist)
    {
        return None;
    }
    if query.duration_s > 0 {
        let recorded = candidate["duration"].as_f64()?;
        if (recorded - f64::from(query.duration_s)).abs() > 3.0 {
            return None;
        }
    }
    if let Some(lines) = candidate["syncedLyrics"]
        .as_str()
        .and_then(|raw| parse_lrc(raw, query.duration_s))
    {
        return Some(Lyrics {
            lines,
            synced: true,
        });
    }
    candidate["plainLyrics"]
        .as_str()
        .and_then(parse_plain)
        .map(|lines| Lyrics {
            lines,
            synced: false,
        })
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(500)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `[mm:ss.xx]` with 1–3 minute digits and an optional 1–3 digit fraction.
fn stamp(row: &str) -> Option<(u64, usize)> {
    let bytes = row.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut i = 1;
    let digits = |i: &mut usize, max: usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() && *i - start < max {
            *i += 1;
        }
        (*i > start)
            .then(|| row[start..*i].parse::<u64>().ok())
            .flatten()
    };
    let minutes = digits(&mut i, 3)?;
    if bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let start = i;
    let seconds = digits(&mut i, 2)?;
    if i - start != 2 || seconds > 59 {
        return None;
    }
    let mut fraction = 0;
    if matches!(bytes.get(i), Some(b'.' | b':')) {
        i += 1;
        let start = i;
        let value = digits(&mut i, 3)?;
        fraction = value * 10u64.pow(3 - (i - start) as u32);
    }
    if bytes.get(i) != Some(&b']') {
        return None;
    }
    Some(((minutes * 60 + seconds) * 1000 + fraction, i + 1))
}

/// Parses bounded LRC text as data. Rejects offsets, out-of-range times and
/// oversized input rather than guessing.
pub fn parse_lrc(raw: &str, duration_s: u32) -> Option<Vec<Line>> {
    if raw.len() > MAX_TEXT_BYTES {
        return None;
    }
    let mut lines = Vec::new();
    for row in raw.lines().take(2000) {
        let row = row.trim_start();
        if row.to_ascii_lowercase().starts_with("[offset:") {
            let value = row[8..].trim_end_matches(']').trim();
            if !value
                .trim_start_matches(['+', '-'])
                .trim_start_matches('0')
                .is_empty()
            {
                return None;
            }
            continue;
        }
        let mut position = 0;
        let mut times = Vec::new();
        while let Some((ms, used)) = stamp(&row[position..]) {
            if duration_s > 0 && ms > (u64::from(duration_s) + 5) * 1000 {
                return None;
            }
            times.push(ms);
            position += used;
        }
        let text = clean(&row[position..]);
        lines.extend(times.into_iter().map(|ms| Line {
            start_ms: Some(ms),
            text: text.clone(),
        }));
        if lines.len() > MAX_LINES {
            return None;
        }
    }
    lines.sort_by_key(|l| l.start_ms);
    // Leading blank stamps only mark the intro.
    while lines.first().is_some_and(|l| l.text.is_empty()) {
        lines.remove(0);
    }
    (lines.iter().any(|l| !l.text.is_empty())).then_some(lines)
}

fn parse_plain(raw: &str) -> Option<Vec<Line>> {
    if raw.len() > MAX_TEXT_BYTES {
        return None;
    }
    let lines: Vec<_> = raw
        .lines()
        .take(MAX_LINES)
        .map(|l| Line {
            start_ms: None,
            text: clean(l),
        })
        .collect();
    lines.iter().any(|l| !l.text.is_empty()).then_some(lines)
}

/// The line sung at `position_ms`, or -1 before the first line, during an
/// instrumental gap or for unsynced lyrics.
pub fn active_line(lines: &[Line], position_ms: f32) -> i32 {
    if !position_ms.is_finite()
        || position_ms < 0.0
        || lines.first().is_none_or(|l| l.start_ms.is_none())
    {
        return -1;
    }
    let end = lines.partition_point(|l| l.start_ms.unwrap_or(u64::MAX) as f32 <= position_ms);
    end.checked_sub(1)
        .filter(|&i| !lines[i].text.is_empty())
        .map_or(-1, |i| i as i32)
}

struct Work {
    key: String,
    receiver: mpsc::Receiver<Result<Option<Lyrics>, String>>,
    handle: std::thread::JoinHandle<()>,
}

struct Runtime {
    fetcher: Fetcher,
    cache: VecDeque<(String, Option<Lyrics>)>,
    key: Option<String>,
    shown: Option<Lyrics>,
    failed: bool,
    worker: Option<Work>,
}

impl Runtime {
    fn cached(&self, key: &str) -> Option<Option<Lyrics>> {
        self.cache
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
    fn remember(&mut self, key: String, value: Option<Lyrics>) {
        self.cache.retain(|(k, _)| *k != key);
        self.cache.push_back((key, value));
        while self.cache.len() > CACHE_TRACKS {
            self.cache.pop_front();
        }
    }
}

pub struct Controller {
    timer: slint::Timer,
    runtime: Rc<RefCell<Runtime>>,
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.timer.stop();
        // A lookup has a short timeout; never block the UI waiting for it.
        drop(self.runtime.borrow_mut().worker.take());
    }
}

/// The cache key for what is playing, if lyrics can be looked up for it.
fn track_key(ui: &GroveWindow) -> Option<String> {
    let uri = ui.get_current_uri();
    if ui.get_local_active() && !ui.get_track_title().is_empty() {
        return Some(format!(
            "local:{}|{}",
            ui.get_track_title(),
            ui.get_artist()
        ));
    }
    uri.starts_with("spotify:track:").then(|| uri.to_string())
}

fn query(ui: &GroveWindow) -> Query {
    let artist = ui.get_artist();
    Query {
        title: ui.get_track_title().to_string(),
        artist: artist.split(", ").next().unwrap_or_default().to_owned(),
        album: ui.get_current_album().to_string(),
        duration_s: (ui.get_duration().max(0.0) / 1000.0).round() as u32,
    }
}

fn show(ui: &GroveWindow, lyrics: Option<&Lyrics>, failed: bool, loading: bool) {
    let rows = lyrics
        .map(|l| {
            l.lines
                .iter()
                .map(|line| GroveLyricLine {
                    text: line.text.clone().into(),
                    timed: line.start_ms.is_some(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has = !rows.is_empty();
    ui.set_lyrics_lines(ModelRc::new(VecModel::from(rows)));
    ui.set_lyrics_active(-1);
    ui.set_lyrics_synced(lyrics.is_some_and(|l| l.synced));
    ui.set_lyrics_state(if loading {
        1
    } else if has {
        2
    } else if failed {
        4
    } else {
        3
    });
    ui.set_lyrics_source(if has { "Lyrics from LRCLIB" } else { "" }.into());
    ui.set_lyrics_status(
        if loading {
            ""
        } else if has && !lyrics.is_some_and(|l| l.synced) {
            "These lyrics aren't time-synced."
        } else if failed {
            "Check your connection, then try again."
        } else if has {
            ""
        } else {
            "LRCLIB has no lyrics for this song yet."
        }
        .into(),
    );
}

fn idle(ui: &GroveWindow) {
    ui.set_lyrics_lines(ModelRc::default());
    ui.set_lyrics_active(-1);
    ui.set_lyrics_synced(false);
    ui.set_lyrics_state(0);
    ui.set_lyrics_source("".into());
    ui.set_lyrics_status("Play a song to see its lyrics.".into());
}

pub fn install(ui: &GroveWindow, fetcher: Fetcher) -> Controller {
    let runtime = Rc::new(RefCell::new(Runtime {
        fetcher,
        cache: VecDeque::new(),
        key: None,
        shown: None,
        failed: false,
        worker: None,
    }));
    idle(ui);
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_lyrics_seek(move |index| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_can_seek() || (ui.get_jam_connected() && !ui.get_jam_host()) {
                return;
            }
            let time = runtime
                .borrow()
                .shown
                .as_ref()
                .and_then(|l| l.lines.get(usize::try_from(index).ok()?))
                .and_then(|l| l.start_ms);
            if let Some(ms) = time {
                ui.invoke_seek(ms as f32);
            }
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_lyrics_retry(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut r = runtime.borrow_mut();
            if let Some(key) = r.key.clone() {
                r.cache.retain(|(k, _)| *k != key);
            }
            r.shown = None;
            r.failed = false;
            r.key = None;
            idle(&ui);
        });
    }
    let timer = slint::Timer::default();
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(120),
            move || {
                let Some(ui) = weak.upgrade() else { return };
                tick(&ui, &mut runtime.borrow_mut());
            },
        );
    }
    Controller { timer, runtime }
}

fn tick(ui: &GroveWindow, r: &mut Runtime) {
    let key = track_key(ui);
    if key != r.key {
        r.key = key.clone();
        r.shown = None;
        r.failed = false;
        idle(ui);
    }
    // A finished lookup is cached even if the song changed meanwhile.
    if r.worker.as_ref().is_some_and(|w| w.handle.is_finished()) {
        let work = r.worker.take().unwrap();
        let _ = work.handle.join();
        let reply = work.receiver.try_recv().unwrap_or(Err(String::new()));
        let current = r.key.as_deref() == Some(work.key.as_str());
        match reply {
            Ok(value) => {
                r.remember(work.key, value.clone());
                if current {
                    r.shown = value;
                    show(ui, r.shown.as_ref(), false, false);
                }
            }
            Err(_) if current => {
                r.failed = true;
                show(ui, None, true, false);
            }
            Err(_) => {}
        }
    }
    let Some(key) = key else { return };
    let wanted = ui.get_now_playing_visible() && ui.window().is_visible();
    if wanted && r.shown.is_none() && !r.failed && ui.get_lyrics_state() != 3 {
        if let Some(cached) = r.cached(&key) {
            r.shown = cached;
            show(ui, r.shown.as_ref(), false, false);
        } else if r.worker.is_none() {
            let query = query(ui);
            if query.title.is_empty() || query.artist.is_empty() {
                return;
            }
            let fetcher = r.fetcher.clone();
            let (tx, receiver) = mpsc::sync_channel(1);
            match std::thread::Builder::new()
                .name("grove-lyrics".into())
                .spawn(move || {
                    let _ = tx.try_send(fetcher(&query));
                }) {
                Ok(handle) => {
                    r.worker = Some(Work {
                        key: key.clone(),
                        receiver,
                        handle,
                    });
                    show(ui, None, false, true);
                }
                Err(_) => {
                    r.failed = true;
                    show(ui, None, true, false);
                }
            }
        }
    }
    if let Some(lyrics) = r.shown.as_ref().filter(|l| l.synced) {
        let active = active_line(&lyrics.lines, ui.get_position());
        if ui.get_lyrics_active() != active {
            ui.set_lyrics_active(active);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(ms: u64, text: &str) -> Line {
        Line {
            start_ms: Some(ms),
            text: text.into(),
        }
    }

    #[test]
    fn parses_lrc_stamps_repeats_and_fractions() {
        let lines = parse_lrc(
            "[ar:Someone]\n[00:01.5]One\n[00:03.250][00:09.00]Chorus\n[00:05]Bare\n[1:02.123]Late",
            200,
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![
                line(1500, "One"),
                line(3250, "Chorus"),
                line(5000, "Bare"),
                line(9000, "Chorus"),
                line(62123, "Late"),
            ]
        );
    }

    #[test]
    fn rejects_offsets_overlong_times_and_oversized_text() {
        assert!(parse_lrc("[offset:+250]\n[00:01.00]A", 100).is_none());
        assert!(parse_lrc("[offset:0]\n[00:01.00]A", 100).is_some());
        assert!(parse_lrc("[09:00.00]Too late", 100).is_none());
        assert!(parse_lrc(&"[00:01.00]x\n".repeat(20_000), 100).is_none());
        assert!(parse_lrc("[00:01.00]\n[00:02.00]", 100).is_none());
        assert_eq!(parse_lrc("[00:01.00]a\u{1b}b", 0).unwrap()[0].text, "ab");
    }

    #[test]
    fn follows_position_gaps_and_unsynced_lyrics() {
        let lines = vec![line(1000, "First"), line(5000, ""), line(7000, "Next")];
        assert_eq!(active_line(&lines, 0.0), -1);
        assert_eq!(active_line(&lines, 1000.0), 0);
        assert_eq!(active_line(&lines, 6000.0), -1);
        assert_eq!(active_line(&lines, 7500.0), 2);
        assert_eq!(active_line(&lines, f32::NAN), -1);
        let plain = parse_plain("One\nTwo").unwrap();
        assert_eq!(active_line(&plain, 1000.0), -1);
    }

    #[test]
    fn accepts_only_close_matches_and_prefers_synced_lyrics() {
        let query = Query {
            title: "Forest Floor - Remastered 2011".into(),
            artist: "Grove Band".into(),
            album: "Rings".into(),
            duration_s: 200,
        };
        let candidate = json!({"trackName":"Forest floor","artistName":"Grove Band feat. Echo","duration":201.0,
            "syncedLyrics":"[00:01.00]Hello","plainLyrics":"Hello"});
        let found = matched(&candidate, &query).unwrap();
        assert!(found.synced);
        let plain = json!({"trackName":"Forest Floor","artistName":"Grove Band","duration":199,"plainLyrics":"Hello\nWorld"});
        assert!(!matched(&plain, &query).unwrap().synced);
        let wrong_length = json!({"trackName":"Forest Floor","artistName":"Grove Band","duration":260,"syncedLyrics":"[00:01.00]Hi"});
        assert!(matched(&wrong_length, &query).is_none());
        let wrong_artist = json!({"trackName":"Forest Floor","artistName":"Someone Else","duration":200,"syncedLyrics":"[00:01.00]Hi"});
        assert!(matched(&wrong_artist, &query).is_none());
        let instrumental = json!({"trackName":"Forest Floor","artistName":"Grove Band","duration":200,"instrumental":true});
        assert!(matched(&instrumental, &query).is_none());
    }

    #[test]
    fn fixture_is_offline_and_has_a_no_lyrics_case() {
        let fetch = fixture();
        let mut query = Query {
            title: "Review song".into(),
            artist: "Grove".into(),
            album: String::new(),
            duration_s: 210,
        };
        assert!(fetch(&query).unwrap().unwrap().synced);
        query.title = "No lyrics here".into();
        assert!(fetch(&query).unwrap().is_none());
    }
}
