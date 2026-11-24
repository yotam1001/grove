//! UI-facing feature controller. Request IDs prevent stale pages replacing navigation.
//! The shared state and request plumbing live here; each area has its own file.
mod appearance;
mod artist;
mod collections;
mod history;
mod jam_match;
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

use navigation::{first_page, PreviousPage};

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
    previous: Option<PreviousPage>,
    jam_pairs: tide_jam::matching::Pairings,
    jam_remote: String,
    jam_candidates: Vec<Track>,
    related: Vec<Track>,
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
            previous: None,
            jam_pairs: Default::default(),
            jam_remote: String::new(),
            jam_candidates: vec![],
            related: vec![],
        }));
        jam_match::bind(ui, &this);
        navigation::bind(ui, &this);
        search::bind(ui, &this);
        collections::bind(ui, &this);
        song_actions::bind(ui, &this);
        appearance::bind(ui, &this);
        queue::bind(ui, &this);
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
        self.serial = self.serial.wrapping_add(1);
        let id = self.serial;
        self.pending.insert(id, pending);
        if !self.offline {
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
            self.art.clear();
            self.rows.clear();
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
        if matches!(pending,Pending::Page{epoch,..} if epoch!=self.epoch) {
            return;
        }
        if matches!(&pending,Pending::Metadata(uri)|Pending::Saved(uri)|Pending::Related(uri) if ui.get_current_uri().as_str()!=uri)
        {
            return;
        }
        if matches!(&pending,Pending::JamAdd{invite,..}|Pending::JamMatch{invite,..} if !ui.get_jam_connected() || invite!=ui.get_jam_invite().as_str())
        {
            return;
        }
        if matches!(&pending,Pending::JamMatch{remote,..} if remote!=&self.jam_remote) {
            return;
        }
        if matches!(pending, Pending::Page { .. }) {
            ui.set_feature_busy(false);
        }
        let data = match result {
            Ok(d) => d,
            Err(e) => {
                if matches!(pending, Pending::JamAdd { .. } | Pending::JamMatch { .. }) {
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
        match pending {
            Pending::JamMatch { .. } => self.jam_matches_loaded(ui, &data),
            Pending::Enqueue => self.enqueued(ui, &data),
            Pending::JamAdd {
                uri,
                invite,
                sender,
            } => self.jam_track_resolved(ui, &data, uri, invite, sender),
            Pending::Page { append, .. } => self.page_loaded(ui, &data, append),
            Pending::Playlists => self.playlists_loaded(ui, &data),
            Pending::Mutation => self.mutation_confirmed(ui, &data),
            Pending::Metadata(uri) => self.metadata_loaded(ui, &data, uri),
            Pending::Saved(uri) => self.saved_loaded(ui, &data, uri),
            Pending::Related(_) => self.related_loaded(ui, &data),
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
                    .unwrap_or_else(|| art.get(&t.artwork_url)),
            })
            .collect::<Vec<_>>();
        ui.set_feature_tracks(ModelRc::new(VecModel::from(rows)));
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
        self.previous = None;
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
        if ui.get_page() == 5 {
            self.load(ui, Action::Home, false);
            return;
        }
        if matches!(ui.get_page(), 5..=7)
            || (ui.get_page() == 3 && !ui.get_collection_uri().is_empty())
        {
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
