//! UI-facing feature controller. Request IDs prevent stale pages replacing navigation.
//! The shared state and request plumbing live here; each area has its own file.
mod appearance;
mod artist;
mod browse;
mod browse_review;
mod collections;
mod discovery;
mod history;
mod home;
mod jam_match;
mod library_tabs;
mod navigation;
mod now_playing;
mod queue;
pub mod review;
mod search;
mod song_actions;
use crate::{
    artwork,
    library::Action,
    preferences::Preferences,
    spotify::{Request, RequestSender},
    state::{self, Track},
    GroveFeatureTrack, GroveWindow,
};
use serde_json::{json, Value};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use navigation::first_page;

#[derive(Clone)]
enum Pending {
    Page {
        epoch: u64,
        append: bool,
    },
    Playlists,
    Mutation,
    Metadata(String),
    Saved(String),
    Related(String),
    Enqueue,
    JamMatch {
        invite: String,
        remote: String,
    },
    JamAdd {
        uri: String,
        invite: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    },
    /// Library tabs, search paging, follows, autoplay refills and batched lookups.
    Browse(browse::Want),
    JamCollection {
        invite: String,
        group: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    },
    JamIsrc {
        uri: String,
    },
}
pub struct Features {
    pub prefs: Preferences,
    offline: bool,
    tx: RequestSender,
    serial: u64,
    epoch: u64,
    view_page: i32,
    pending: HashMap<u64, Pending>,
    source: Option<Action>,
    pub rows: Vec<Track>,
    art: artwork::Cache,
    metadata: Track,
    pub queue: Vec<Track>,
    starting: Option<(String, Option<u64>, Instant)>,
    rotation: Vec<Track>,
    played: Option<(String, Instant)>,
    last_tick: Instant,
    listened: Duration,
    pub history: Vec<Track>,
    browse: browse::Browse,
    jam_pairs: tide_jam::matching::Pairings,
    jam_remote: String,
    jam_candidates: Vec<Track>,
    related: Vec<Track>,
    jam: jam_match::JamState,
}
impl Features {
    pub fn install(ui: &GroveWindow, tx: RequestSender, offline: bool) -> Rc<RefCell<Self>> {
        let prefs = appearance::load_preferences(ui, offline);
        let this = Rc::new(RefCell::new(Self {
            prefs,
            offline,
            tx,
            serial: 0,
            epoch: 0,
            view_page: 0,
            pending: HashMap::new(),
            source: None,
            rows: vec![],
            art: artwork::Cache::new(offline),
            metadata: Track::default(),
            queue: vec![],
            starting: None,
            rotation: vec![],
            played: None,
            last_tick: Instant::now(),
            listened: Duration::ZERO,
            history: vec![],
            browse: browse::Browse::new(ui, offline),
            jam_pairs: Default::default(),
            jam_remote: String::new(),
            jam_candidates: vec![],
            related: vec![],
            jam: Default::default(),
        }));
        jam_match::bind(ui, &this);
        navigation::bind(ui, &this);
        search::bind(ui, &this);
        collections::bind(ui, &this);
        song_actions::bind(ui, &this);
        appearance::bind(ui, &this);
        queue::bind(ui, &this);
        browse::bind(ui, &this);
        this
    }
    fn request(&mut self, action: Action, pending: Pending) -> Option<u64> {
        if self.pending.len() >= 8 {
            return None;
        }
        if matches!(pending, Pending::Playlists)
            && self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Playlists))
        {
            return None;
        }
        // Identical reads already in flight are collapsed into one request.
        let key = action.read_key();
        let pending_ids = &self.pending;
        self.browse
            .keys
            .retain(|id, _| pending_ids.contains_key(id));
        if let Some(key) = &key {
            let same = std::mem::discriminant(&pending);
            if let Some((&id, _)) = self.browse.keys.iter().find(|(id, k)| {
                *k == key
                    && self
                        .pending
                        .get(id)
                        .is_some_and(|p| std::mem::discriminant(p) == same)
            }) {
                return Some(id);
            }
        }
        self.serial = self.serial.wrapping_add(1);
        let id = self.serial;
        self.pending.insert(id, pending);
        if let Some(key) = key {
            self.browse.keys.insert(id, key);
        }
        // Track lookups wait for the next poll so they can share one request.
        if self.offline {
            // Review modes never reach Spotify; they check what would have been sent.
            self.browse.sent.push(action.clone());
            if self.browse.sent.len() > 64 {
                self.browse.sent.remove(0);
            }
        }
        if let Action::TrackInfo { uri } = action {
            self.browse.batch.push((id, uri));
        } else if !self.offline {
            let _ = self.tx.send(Request::Feature { id, action });
        }
        Some(id)
    }
    fn load(&mut self, ui: &GroveWindow, action: Action, append: bool) {
        if !ui.get_library_ready() {
            ui.set_feature_message("Connect Spotify to browse your music.".into());
            return;
        }
        if append && ui.get_feature_busy() {
            return;
        }
        self.view_page = ui.get_page();
        // A new initial page invalidates an older request, including search A -> B.
        if !append {
            self.epoch += 1;
            self.art.cancel();
            self.rows.clear();
            self.browse.positions.clear();
            self.browse.removal = None;
            ui.set_feature_more(false);
            self.render(ui);
        }
        let pending = Pending::Page {
            epoch: self.epoch,
            append,
        };
        if self.request(action.clone(), pending).is_some() {
            self.source = Some(action);
            ui.set_feature_busy(true);
            ui.set_feature_message("Loading from Spotify…".into());
        } else {
            ui.set_feature_message("Wait for the current requests to finish.".into());
        }
    }
    pub fn response(&mut self, ui: &GroveWindow, id: u64, result: Result<Value, String>) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        self.browse.keys.remove(&id);
        if let Pending::Browse(browse::Want::Batch(waiting)) = pending {
            return self.batch_loaded(ui, waiting, result);
        }
        if matches!(pending,Pending::Page{epoch,..}|Pending::Browse(browse::Want::LibraryTab{epoch,..}|browse::Want::SearchKind{epoch}) if epoch!=self.epoch)
        {
            return;
        }
        if matches!(&pending,Pending::Metadata(uri)|Pending::Saved(uri)|Pending::Related(uri) if ui.get_current_uri().as_str()!=uri)
        {
            return;
        }
        if matches!(&pending,Pending::JamAdd{invite,..}|Pending::JamMatch{invite,..}|Pending::JamCollection{invite,..} if !ui.get_jam_connected() || invite!=ui.get_jam_invite().as_str())
        {
            return;
        }
        if matches!(&pending,Pending::JamMatch{remote,..} if remote!=&self.jam_remote) {
            return;
        }
        if matches!(
            pending,
            Pending::Page { .. }
                | Pending::Browse(
                    browse::Want::LibraryTab { .. } | browse::Want::SearchKind { .. }
                )
        ) {
            ui.set_feature_busy(false);
        }
        let data = match result {
            Ok(d) if d["kind"] == "throttled" => return self.throttled(ui, pending, &d),
            Ok(d) => d,
            Err(e) => {
                if let Pending::Browse(want) = pending {
                    self.browse_failed(ui, want, e);
                } else if matches!(pending, Pending::JamIsrc { .. }) {
                    self.jam.isrc_failed(&pending);
                } else if matches!(
                    pending,
                    Pending::JamAdd { .. }
                        | Pending::JamMatch { .. }
                        | Pending::JamCollection { .. }
                ) {
                    ui.set_jam_message(e.into());
                } else if matches!(pending, Pending::Related(_)) {
                    self.related_failed(ui, &e);
                } else if matches!(pending, Pending::Enqueue) {
                    ui.set_message(e.into());
                } else {
                    ui.set_feature_message(e.into());
                }
                return;
            }
        };
        if data["kind"] == "track_info" {
            self.browse.note_track(&data["track"]);
        }
        match pending {
            Pending::JamMatch { .. } => self.jam_matches_loaded(ui, &data),
            Pending::Enqueue => self.enqueued(ui, &data),
            Pending::JamAdd {
                uri,
                invite,
                sender,
            } => self.jam_track_resolved(ui, &data, uri, invite, sender),
            Pending::JamCollection { group, sender, .. } => {
                self.jam_collection_loaded(ui, &data, group, sender)
            }
            Pending::JamIsrc { uri } => self.jam.isrc_loaded(&data, uri),
            Pending::Page { append, .. } => self.page_loaded(ui, &data, append),
            Pending::Playlists => self.playlists_loaded(ui, &data),
            Pending::Mutation => self.mutation_confirmed(ui, &data),
            Pending::Metadata(uri) => self.metadata_loaded(ui, &data, uri),
            Pending::Saved(uri) => self.saved_loaded(ui, &data, uri),
            Pending::Related(_) => self.related_loaded(ui, &data),
            Pending::Browse(want) => self.browse_loaded(ui, want, &data),
        }
    }
    fn render(&mut self, ui: &GroveWindow) {
        let previous: HashMap<_, _> = ui
            .get_feature_tracks()
            .iter()
            .filter(|t| t.cover.size().width > 0)
            .map(|t| (t.uri, t.cover))
            .collect();
        let hidden = &self.prefs.hidden;
        let art = &mut self.art;
        // Bounded cover requests per page keep the artwork cache from churning.
        let mut budget = artwork::PAGE_LIMIT;
        let rows = self
            .rows
            .iter()
            .filter(|t| !hidden.contains(&t.uri))
            .map(|t| GroveFeatureTrack {
                title: t.title.clone().into(),
                artist: t.artist.clone().into(),
                duration: state::clock(t.duration).into(),
                uri: t.uri.clone().into(),
                artist_uri: t.artist_uri.clone().into(),
                cover: previous
                    .get(&slint::SharedString::from(t.uri.as_str()))
                    .cloned()
                    .unwrap_or_else(|| {
                        if budget == 0 {
                            return art.peek(&t.artwork_url);
                        }
                        budget -= 1;
                        art.get(&t.artwork_url)
                    }),
            })
            .collect::<Vec<_>>();
        ui.set_feature_tracks(ModelRc::new(VecModel::from(rows)));
        self.render_browse(ui);
    }
    pub fn clear(&mut self, ui: &GroveWindow) {
        self.reset_jam(ui);
        self.epoch += 1;
        self.pending.clear();
        self.rows.clear();
        self.queue.clear();
        self.rotation.clear();
        self.history.clear();
        self.starting = None;
        self.source = None;
        self.reset_browse(ui);
        self.art.clear();
        self.metadata = Track::default();
        ui.set_current_cover(slint::Image::default());
        ui.set_current_uri("".into());
        ui.set_current_saved(false);
        ui.set_feature_busy(false);
        ui.set_feature_more(false);
        ui.set_feature_message("".into());
        ui.set_edit_track("".into());
        self.render(ui);
        self.render_queue(ui);
    }
    pub fn grant_ready(&mut self, ui: &GroveWindow) {
        self.invalidate(ui);
        match ui.get_page() {
            browse::HOME => return self.show_home(ui, false),
            browse::LIBRARY => {
                let tab = ui.global::<crate::GroveBrowse>().get_library_tab();
                return self.library_tab(ui, tab, false);
            }
            _ => {}
        }
        if matches!(ui.get_page(), 6 | 7 | browse::COLLECTION | browse::ARTIST) {
            if let Some(action) = self.source.clone() {
                self.load(ui, first_page(action), false);
            }
        }
    }
    pub fn invalidate(&mut self, ui: &GroveWindow) {
        self.epoch += 1;
        self.pending.clear();
        ui.set_feature_busy(false);
    }
}

impl Features {
    /// A 429 reached the UI. Pages and tabs get the cooldown banner with Retry;
    /// background lookups stay quiet. Playback is never affected.
    fn throttled(&mut self, ui: &GroveWindow, pending: Pending, data: &Value) {
        match pending {
            Pending::Page { .. } => {
                let retry = self.source.clone().map(browse::Retry::Page);
                self.show_cooldown(ui, data, retry);
                ui.set_feature_message("".into());
            }
            Pending::Browse(browse::Want::LibraryTab { tab, .. }) => {
                self.library_throttled(ui, tab, data)
            }
            Pending::Browse(browse::Want::SearchKind { .. }) => self.show_cooldown(ui, data, None),
            Pending::Metadata(_) | Pending::Saved(_) | Pending::Browse(browse::Want::Refill) => {}
            Pending::JamAdd { .. } | Pending::JamMatch { .. } => {
                ui.set_jam_message(data["message"].as_str().unwrap_or_default().into())
            }
            _ => ui.set_message(data["message"].as_str().unwrap_or_default().into()),
        }
    }

    fn browse_loaded(&mut self, ui: &GroveWindow, want: browse::Want, data: &Value) {
        match want {
            browse::Want::LibraryTab { tab, append, .. } => {
                self.library_loaded(ui, tab, append, data, false)
            }
            browse::Want::SearchKind { .. } => self.search_kind_loaded(ui, data),
            browse::Want::Follow { uri, follow } => self.follow_confirmed(ui, &uri, follow),
            browse::Want::Refill => self.refill_loaded(data),
            browse::Want::Batch(_) => {}
        }
    }

    fn browse_failed(&mut self, ui: &GroveWindow, want: browse::Want, error: String) {
        match want {
            browse::Want::LibraryTab { tab, .. } => self.library_failed(ui, tab, error),
            browse::Want::Refill => {}
            _ => ui.set_feature_message(error.into()),
        }
    }
}
