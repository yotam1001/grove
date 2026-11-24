//! Browse state shared by Home, Search, Library tabs, collection and artist pages.
//!
//! Models are kept and updated in place: when artwork arrives only the changed
//! rows are replaced, so shelves keep their paging, menus stay open and covers
//! never flash back to placeholders.
use super::{Features, Pending};
use crate::{
    artwork,
    browse_cache::BrowseCache,
    library::Action,
    state::{self, Track},
    GroveBrowse, GroveCard, GroveHeader, GroveRow, GroveShelf, GroveWindow,
};
use serde_json::Value;
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Instant,
};

pub(super) const LIBRARY: i32 = 3;
pub(super) const HOME: i32 = 5;
pub(super) const SEARCH: i32 = 6;
pub(super) const COLLECTION: i32 = 10;
pub(super) const ARTIST: i32 = 11;

/// A card's source data; covers are looked up at render time.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Entry {
    pub kind: String,
    pub uri: String,
    pub title: String,
    pub subtitle: String,
    pub image: String,
    pub index: i32,
}
impl Entry {
    pub fn from_card(card: &Value) -> Option<Self> {
        Some(Self {
            kind: card["kind"].as_str()?.to_owned(),
            uri: card["uri"].as_str()?.to_owned(),
            title: card["title"].as_str()?.to_owned(),
            subtitle: card["subtitle"].as_str().unwrap_or_default().to_owned(),
            image: card["image"].as_str().unwrap_or_default().to_owned(),
            index: -1,
        })
    }
    pub fn from_cards(cards: &Value) -> Vec<Self> {
        cards
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Self::from_card)
            .collect()
    }
    pub fn track(track: &Track, index: usize) -> Self {
        Self {
            kind: "track".into(),
            uri: track.uri.clone(),
            title: track.title.clone(),
            subtitle: track.artist.clone(),
            image: track.artwork_url.clone(),
            index: index as i32,
        }
    }
    pub fn liked() -> Self {
        Self {
            kind: "liked".into(),
            uri: "liked".into(),
            title: "Liked Songs".into(),
            subtitle: "Your saved songs".into(),
            ..Default::default()
        }
    }
    pub fn mix() -> Self {
        Self {
            kind: "mix".into(),
            uri: "mix".into(),
            title: "Discovery mix".into(),
            subtitle: "Made for you from your listening".into(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Header {
    pub kind: String,
    pub label: String,
    pub title: String,
    pub subtitle: String,
    pub uri: String,
    pub image: String,
    pub owned: bool,
    pub can_follow: bool,
    pub following: bool,
    pub note: String,
}

#[derive(Clone)]
pub(super) enum Want {
    LibraryTab { tab: i32, append: bool, epoch: u64 },
    SearchKind { epoch: u64 },
    Follow { uri: String, follow: bool },
    Refill,
    Batch(Vec<(u64, String)>),
}

pub(super) enum Retry {
    Page(Action),
    Tab(i32),
}
pub(super) struct Cooldown {
    pub until: Instant,
    pub quota: bool,
    pub message: String,
    pub retry: Option<Retry>,
}

/// What Back restores.
#[derive(Clone)]
pub(super) struct Snapshot {
    pub page: i32,
    pub rows: Vec<Track>,
    pub positions: Vec<u32>,
    pub source: Option<Action>,
    pub title: SharedString,
    pub more: bool,
    pub header: Header,
    pub shelves: Vec<Shelf>,
    pub credited: Vec<Entry>,
    pub collection: (SharedString, SharedString, bool),
}

pub(super) type Shelf = (String, String, Vec<Entry>);

struct ShelfModels {
    outer: Rc<VecModel<GroveShelf>>,
    inner: Vec<Rc<VecModel<GroveCard>>>,
}
impl ShelfModels {
    fn new() -> Self {
        Self {
            outer: Rc::new(VecModel::default()),
            inner: vec![],
        }
    }
    fn sync(&mut self, shelves: Vec<(String, String, Vec<GroveCard>)>) {
        let same = self.outer.row_count() == shelves.len()
            && shelves.iter().enumerate().all(|(i, (id, title, _))| {
                self.outer
                    .row_data(i)
                    .is_some_and(|s| s.id == id.as_str() && s.title == title.as_str())
            });
        if same {
            for (model, (_, _, cards)) in self.inner.iter().zip(shelves) {
                sync(model, cards);
            }
            return;
        }
        self.inner = shelves
            .iter()
            .map(|(_, _, cards)| Rc::new(VecModel::from(cards.clone())))
            .collect();
        self.outer.set_vec(
            shelves
                .iter()
                .zip(&self.inner)
                .map(|((id, title, _), model)| GroveShelf {
                    id: id.into(),
                    title: title.into(),
                    cards: ModelRc::from(model.clone()),
                })
                .collect::<Vec<_>>(),
        );
    }
}

struct Models {
    rows: Rc<VecModel<GroveRow>>,
    search_songs: Rc<VecModel<GroveRow>>,
    picks: Rc<VecModel<GroveCard>>,
    library: Rc<VecModel<GroveCard>>,
    search_cards: Rc<VecModel<GroveCard>>,
    credited: Rc<VecModel<GroveCard>>,
    choices: Rc<VecModel<GroveCard>>,
    home: ShelfModels,
    search: ShelfModels,
    artist: ShelfModels,
}

/// Updates a model in place when its length is unchanged, so Slint keeps the
/// existing elements (and their hover, paging and menu state).
pub(super) fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
    } else {
        model.set_vec(rows);
    }
}

pub(super) struct Browse {
    pub cache: BrowseCache,
    pub playlists: Vec<Entry>,
    pub home_shelves: Vec<Shelf>,
    pub hero_label: String,
    pub albums: Vec<Entry>,
    pub albums_more: bool,
    pub artists: Vec<Entry>,
    pub artists_after: Option<String>,
    pub remote_recent: Vec<Track>,
    pub search: Option<Value>,
    pub search_cards: Vec<Entry>,
    pub search_cards_more: bool,
    pub header: Header,
    pub artist_shelves: Vec<Shelf>,
    pub credited: Vec<Entry>,
    pub credits: HashMap<String, Vec<(String, String)>>,
    pub albums_of: HashMap<String, String>,
    pub saved: HashSet<String>,
    /// Playlist position of each row in `Features::rows` (collection pages).
    pub positions: Vec<u32>,
    /// The row a confirmed "Remove from playlist" will remove.
    pub removal: Option<usize>,
    pub cooldown: Option<Cooldown>,
    pub stack: Vec<Snapshot>,
    /// Recently left collection and artist pages, so Back/Forward reopen them
    /// without another request.
    pub visited: Vec<(Instant, Snapshot)>,
    pub batch: Vec<(u64, String)>,
    pub keys: HashMap<u64, String>,
    pub play_on_load: bool,
    pub seeds: Vec<String>,
    pub heard: HashSet<String>,
    pub refill_at: Option<Instant>,
    /// Offline review modes only: the requests Grove would have sent.
    pub sent: Vec<Action>,
    models: Models,
}

impl Browse {
    pub fn new(ui: &GroveWindow, offline: bool) -> Self {
        let models = Models {
            rows: Rc::new(VecModel::default()),
            search_songs: Rc::new(VecModel::default()),
            picks: Rc::new(VecModel::default()),
            library: Rc::new(VecModel::default()),
            search_cards: Rc::new(VecModel::default()),
            credited: Rc::new(VecModel::default()),
            choices: Rc::new(VecModel::default()),
            home: ShelfModels::new(),
            search: ShelfModels::new(),
            artist: ShelfModels::new(),
        };
        let g = ui.global::<GroveBrowse>();
        g.set_rows(ModelRc::from(models.rows.clone()));
        g.set_search_songs(ModelRc::from(models.search_songs.clone()));
        g.set_picks(ModelRc::from(models.picks.clone()));
        g.set_library_items(ModelRc::from(models.library.clone()));
        g.set_search_cards(ModelRc::from(models.search_cards.clone()));
        g.set_credited(ModelRc::from(models.credited.clone()));
        g.set_artist_choices(ModelRc::from(models.choices.clone()));
        g.set_home_shelves(ModelRc::from(models.home.outer.clone()));
        g.set_search_shelves(ModelRc::from(models.search.outer.clone()));
        g.set_artist_shelves(ModelRc::from(models.artist.outer.clone()));
        let _ = offline;
        Self {
            cache: BrowseCache::memory(),
            playlists: vec![],
            home_shelves: vec![],
            hero_label: String::new(),
            albums: vec![],
            albums_more: false,
            artists: vec![],
            artists_after: None,
            remote_recent: vec![],
            search: None,
            search_cards: vec![],
            search_cards_more: false,
            header: Header::default(),
            artist_shelves: vec![],
            credited: vec![],
            credits: HashMap::new(),
            albums_of: HashMap::new(),
            saved: HashSet::new(),
            positions: vec![],
            removal: None,
            cooldown: None,
            stack: vec![],
            visited: vec![],
            batch: vec![],
            keys: HashMap::new(),
            play_on_load: false,
            seeds: vec![],
            heard: HashSet::new(),
            refill_at: None,
            sent: vec![],
            models,
        }
    }

    /// Records credits and album names so the table and artist choice can use them.
    pub fn note_track(&mut self, item: &Value) {
        let Some(uri) = item["uri"].as_str() else {
            return;
        };
        let artists: Vec<(String, String)> = item["artists"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                Some((
                    a["name"].as_str()?.to_owned(),
                    a["uri"].as_str()?.to_owned(),
                ))
            })
            .filter(|(_, u)| u.starts_with("spotify:artist:"))
            .take(8)
            .collect();
        if self.credits.len() < 5000 {
            self.credits.insert(uri.to_owned(), artists);
        }
        if let Some(album) = item["album"]["name"].as_str() {
            if self.albums_of.len() < 5000 {
                self.albums_of.insert(uri.to_owned(), album.to_owned());
            }
        }
    }
}

/// Parses tracks from any browse response. Collection items carry playlist positions.
pub(super) fn parse_tracks(browse: &mut Browse, data: &Value) -> (Vec<Track>, Vec<u32>) {
    let collection = data["kind"] == "collection_page";
    let items = if collection {
        &data["items"]
    } else {
        &data["tracks"]
    };
    let offset = data["offset"].as_u64().unwrap_or_default() as u32;
    let mut rows = vec![];
    let mut positions = vec![];
    for (j, v) in items.as_array().into_iter().flatten().enumerate() {
        let item = if collection {
            match v.get("item").or_else(|| v.get("track")) {
                Some(item) => item,
                None => continue,
            }
        } else {
            v
        };
        if !item["uri"]
            .as_str()
            .is_some_and(|u| u.starts_with("spotify:track:"))
        {
            continue;
        }
        browse.note_track(item);
        rows.push(Track::from_entity(&crate::web_api::entity(item)));
        positions.push(offset.saturating_add(j as u32));
    }
    (rows, positions)
}

impl Features {
    fn cover(art: &mut artwork::Cache, url: &str, budget: &mut usize) -> Image {
        if url.is_empty() {
            return Image::default();
        }
        if *budget == 0 {
            return art.peek(url);
        }
        *budget -= 1;
        art.get(url)
    }

    fn cards(art: &mut artwork::Cache, entries: &[Entry], budget: &mut usize) -> Vec<GroveCard> {
        entries
            .iter()
            .map(|e| GroveCard {
                kind: e.kind.clone().into(),
                uri: e.uri.clone().into(),
                title: e.title.clone().into(),
                subtitle: e.subtitle.clone().into(),
                cover: {
                    let cover = Self::cover(art, &e.image, budget);
                    if e.kind == "artist" {
                        art.round(&e.image, cover)
                    } else {
                        cover
                    }
                },
                index: e.index,
            })
            .collect()
    }

    /// Keeps a cover already on screen when the cache no longer holds it.
    fn keep_cards(model: &VecModel<GroveCard>, mut cards: Vec<GroveCard>) -> Vec<GroveCard> {
        for (i, card) in cards.iter_mut().enumerate() {
            if card.cover.size().width == 0 {
                if let Some(old) = model.row_data(i).filter(|o| o.uri == card.uri) {
                    card.cover = old.cover;
                }
            }
        }
        cards
    }

    pub(super) fn table_rows(
        &mut self,
        ui: &GroveWindow,
        filter: &str,
        limit: usize,
    ) -> Vec<GroveRow> {
        let needle = filter.trim().to_lowercase();
        let current = ui.get_current_uri();
        let liked = matches!(&self.source, Some(Action::Collection { uri, .. }) if uri == "liked");
        let mut budget = artwork::PAGE_LIMIT;
        let mut out = vec![];
        for (i, t) in self.rows.iter().enumerate() {
            if self.prefs.hidden.contains(&t.uri) {
                continue;
            }
            let album = self
                .browse
                .albums_of
                .get(&t.uri)
                .cloned()
                .unwrap_or_default();
            if !needle.is_empty()
                && ![&t.title, &t.artist, &album]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&needle))
            {
                continue;
            }
            if out.len() >= limit {
                break;
            }
            let saved = liked
                || self.browse.saved.contains(&t.uri)
                || (current == t.uri.as_str() && ui.get_current_saved());
            out.push(GroveRow {
                index: i as i32,
                number: (out.len() + 1).to_string().into(),
                title: t.title.clone().into(),
                artist: t.artist.clone().into(),
                album: album.into(),
                duration: state::clock(t.duration).into(),
                uri: t.uri.clone().into(),
                cover: Self::cover(&mut self.art, &t.artwork_url, &mut budget),
                saved,
                playing: current == t.uri.as_str(),
                artists: self
                    .browse
                    .credits
                    .get(&t.uri)
                    .map_or(1, |c| c.len().max(1)) as i32,
            });
        }
        out
    }

    fn keep_rows(model: &VecModel<GroveRow>, mut rows: Vec<GroveRow>) -> Vec<GroveRow> {
        for (i, row) in rows.iter_mut().enumerate() {
            if row.cover.size().width == 0 {
                if let Some(old) = model.row_data(i).filter(|o| o.uri == row.uri) {
                    row.cover = old.cover;
                }
            }
        }
        rows
    }

    /// Pushes the current page's browse data into the window's models.
    pub(super) fn render_browse(&mut self, ui: &GroveWindow) {
        let page = ui.get_page();
        let g = ui.global::<GroveBrowse>();
        if matches!(page, LIBRARY | SEARCH | COLLECTION | ARTIST) {
            let filter = if page == LIBRARY {
                g.get_library_filter().to_string()
            } else {
                String::new()
            };
            let rows = self.table_rows(ui, &filter, usize::MAX);
            let rows = Self::keep_rows(&self.browse.models.rows, rows);
            sync(&self.browse.models.rows, rows);
        }
        match page {
            HOME => self.render_home(ui),
            SEARCH => self.render_search(ui),
            LIBRARY => self.render_library(ui),
            COLLECTION | ARTIST => {
                let h = &self.browse.header;
                let mut budget = 4;
                let mut cover = Self::cover(&mut self.art, &h.image.clone(), &mut budget);
                if h.kind == "artist" {
                    cover = self.art.round(&h.image.clone(), cover);
                }
                let old = g.get_header();
                let header = GroveHeader {
                    kind: h.kind.clone().into(),
                    label: h.label.clone().into(),
                    title: h.title.clone().into(),
                    subtitle: h.subtitle.clone().into(),
                    uri: h.uri.clone().into(),
                    cover: if cover.size().width == 0 && old.uri == h.uri.as_str() {
                        old.cover
                    } else {
                        cover
                    },
                    owned: h.owned,
                    can_follow: h.can_follow,
                    following: h.following,
                    note: h.note.clone().into(),
                };
                if g.get_header() != header {
                    g.set_header(header);
                }
                let mut budget = 40;
                let shelves = self
                    .browse
                    .artist_shelves
                    .iter()
                    .map(|(id, title, entries)| {
                        (
                            id.clone(),
                            title.clone(),
                            Self::cards(&mut self.art, entries, &mut budget),
                        )
                    })
                    .collect();
                self.browse.models.artist.sync(shelves);
                let credited = Self::cards(&mut self.art, &self.browse.credited.clone(), &mut 0);
                sync(&self.browse.models.credited, credited);
            }
            _ => {}
        }
    }

    pub(super) fn render_home(&mut self, ui: &GroveWindow) {
        let g = ui.global::<GroveBrowse>();
        let mut budget = artwork::PAGE_LIMIT;
        let visible: Vec<(usize, &Track)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, t)| !self.prefs.hidden.contains(&t.uri))
            .take(7)
            .collect();
        let hero = visible
            .first()
            .map(|(i, t)| Entry::track(t, *i))
            .unwrap_or_default();
        let picks: Vec<Entry> = visible
            .iter()
            .skip(1)
            .map(|(i, t)| Entry::track(t, *i))
            .collect();
        let mut hero_card = Self::cards(&mut self.art, &[hero], &mut budget).remove(0);
        let old = g.get_hero();
        if hero_card.cover.size().width == 0 && old.uri == hero_card.uri {
            hero_card.cover = old.cover;
        }
        if g.get_hero() != hero_card {
            g.set_hero(hero_card);
        }
        g.set_hero_label(self.browse.hero_label.clone().into());
        let picks = Self::cards(&mut self.art, &picks, &mut budget);
        let picks = Self::keep_cards(&self.browse.models.picks, picks);
        sync(&self.browse.models.picks, picks);
        let shelves: Vec<Shelf> = self.home_shelf_list();
        let shelves = shelves
            .into_iter()
            .map(|(id, title, entries)| {
                let cards = Self::cards(&mut self.art, &entries, &mut budget);
                (id, title, cards)
            })
            .collect();
        self.browse.models.home.sync(shelves);
    }

    pub(super) fn render_search(&mut self, ui: &GroveWindow) {
        let g = ui.global::<GroveBrowse>();
        let mut budget = artwork::PAGE_LIMIT;
        let Some(data) = self.browse.search.clone() else {
            sync(&self.browse.models.search_songs, vec![]);
            self.browse.models.search.sync(vec![]);
            sync(&self.browse.models.search_cards, vec![]);
            g.set_search_top(GroveCard::default());
            return;
        };
        let songs: Vec<GroveRow> = self.table_rows(ui, "", 4);
        let songs = Self::keep_rows(&self.browse.models.search_songs, songs);
        sync(&self.browse.models.search_songs, songs);
        let top = data["top"].clone();
        let mut entry = Entry::from_card(&top).unwrap_or_default();
        if entry.kind == "track" {
            entry.index = self
                .rows
                .iter()
                .position(|t| t.uri == entry.uri)
                .map_or(-1, |i| i as i32);
        }
        let mut top_card = Self::cards(&mut self.art, &[entry], &mut budget).remove(0);
        let old = g.get_search_top();
        if top_card.cover.size().width == 0 && old.uri == top_card.uri {
            top_card.cover = old.cover;
        }
        if g.get_search_top() != top_card {
            g.set_search_top(top_card);
        }
        let shelves: Vec<_> = [
            ("artists", "Artists"),
            ("albums", "Albums"),
            ("playlists", "Playlists"),
        ]
        .into_iter()
        .filter_map(|(key, title)| {
            let entries = Entry::from_cards(&data[key]);
            (!entries.is_empty()).then(|| {
                (
                    key.to_owned(),
                    title.to_owned(),
                    Self::cards(&mut self.art, &entries, &mut budget),
                )
            })
        })
        .collect();
        self.browse.models.search.sync(shelves);
        let cards = Self::cards(
            &mut self.art,
            &self.browse.search_cards.clone(),
            &mut budget,
        );
        let cards = Self::keep_cards(&self.browse.models.search_cards, cards);
        sync(&self.browse.models.search_cards, cards);
        g.set_search_cards_more(self.browse.search_cards_more);
    }

    pub(super) fn render_library(&mut self, ui: &GroveWindow) {
        let g = ui.global::<GroveBrowse>();
        let needle = g.get_library_filter().trim().to_lowercase();
        let playlists: Vec<Entry> = if self.browse.playlists.is_empty() {
            // Playlists the window received without their artwork details.
            ui.get_playlists()
                .iter()
                .map(|p| Entry {
                    kind: "playlist".into(),
                    uri: p.uri.to_string(),
                    title: p.title.to_string(),
                    subtitle: p.subtitle.to_string(),
                    ..Default::default()
                })
                .collect()
        } else {
            self.browse.playlists.clone()
        };
        let entries: Vec<Entry> = match g.get_library_tab() {
            0 => [Entry::liked(), Entry::mix()]
                .into_iter()
                .chain(playlists)
                .collect(),
            1 => self.browse.albums.clone(),
            2 => self.browse.artists.clone(),
            _ => vec![],
        };
        let entries: Vec<Entry> = entries
            .into_iter()
            .filter(|e| {
                needle.is_empty()
                    || e.title.to_lowercase().contains(&needle)
                    || e.subtitle.to_lowercase().contains(&needle)
            })
            .take(2000)
            .collect();
        let mut budget = artwork::PAGE_LIMIT;
        let cards = Self::cards(&mut self.art, &entries, &mut budget);
        let cards = Self::keep_cards(&self.browse.models.library, cards);
        sync(&self.browse.models.library, cards);
        g.set_library_items_more(match g.get_library_tab() {
            1 => self.browse.albums_more,
            2 => self.browse.artists_after.is_some(),
            _ => false,
        });
    }

    pub(super) fn set_choices(&mut self, ui: &GroveWindow, artists: &[(String, String)]) {
        let entries: Vec<Entry> = artists
            .iter()
            .map(|(name, uri)| Entry {
                kind: "artist".into(),
                uri: uri.clone(),
                title: name.clone(),
                ..Default::default()
            })
            .collect();
        let cards = Self::cards(&mut self.art, &entries, &mut 0);
        sync(&self.browse.models.choices, cards);
        let _ = ui;
    }

    /// Back stack: remember the page being left (except transient duplicates).
    fn snapshot(&self, ui: &GroveWindow, page: i32) -> Snapshot {
        Snapshot {
            page,
            rows: self.rows.clone(),
            positions: self.browse.positions.clone(),
            source: self.source.clone(),
            title: ui.get_feature_title(),
            more: ui.get_feature_more(),
            header: self.browse.header.clone(),
            shelves: self.browse.artist_shelves.clone(),
            credited: self.browse.credited.clone(),
            collection: (
                ui.get_collection_uri(),
                ui.get_collection_title(),
                ui.get_collection_owned(),
            ),
        }
    }

    pub(super) fn push_snapshot(&mut self, ui: &GroveWindow) {
        let page = ui.get_page();
        if page == 7 && self.browse.stack.last().is_some_and(|s| s.page == 7) {
            return;
        }
        self.keep_visited(ui, page);
        let snapshot = self.snapshot(ui, page);
        self.browse.stack.push(snapshot);
        if self.browse.stack.len() > 12 {
            self.browse.stack.remove(0);
        }
    }

    /// Remembers a loaded collection or artist page being left.
    pub(super) fn keep_visited(&mut self, ui: &GroveWindow, page: i32) {
        let uri = self.browse.header.uri.clone();
        if !matches!(page, COLLECTION | ARTIST)
            || uri.is_empty()
            || ui.get_feature_busy()
            || (self.rows.is_empty() && self.browse.artist_shelves.is_empty())
        {
            return;
        }
        let snapshot = self.snapshot(ui, page);
        self.browse.visited.retain(|(_, s)| s.header.uri != uri);
        self.browse.visited.push((Instant::now(), snapshot));
        if self.browse.visited.len() > 8 {
            self.browse.visited.remove(0);
        }
    }

    /// Reopens a page left in the last ten minutes. Returns false to load it.
    pub(super) fn reopen(&mut self, ui: &GroveWindow, uri: &str) -> bool {
        let Some(index) = self.browse.visited.iter().position(|(at, s)| {
            s.header.uri == uri && at.elapsed() < std::time::Duration::from_secs(600)
        }) else {
            return false;
        };
        let (_, s) = self.browse.visited.remove(index);
        self.epoch += 1;
        self.view_page = s.page;
        self.rows = s.rows;
        self.browse.positions = s.positions;
        self.source = s.source;
        self.browse.header = s.header;
        self.browse.artist_shelves = s.shelves;
        self.browse.credited = s.credited;
        let (curi, title, owned) = s.collection;
        ui.set_collection_uri(curi);
        ui.set_collection_title(title);
        ui.set_collection_owned(owned);
        ui.set_page(s.page);
        ui.set_feature_title(s.title);
        ui.set_feature_more(s.more);
        ui.set_feature_busy(false);
        ui.set_feature_message("".into());
        self.render(ui);
        if std::mem::take(&mut self.browse.play_on_load) && !self.rows.is_empty() {
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_play_selection(false);
                }
            });
        }
        true
    }

    pub(super) fn reset_browse(&mut self, ui: &GroveWindow) {
        let offline = self.offline;
        if !offline {
            self.browse.cache.wipe();
        }
        let keep = std::mem::replace(&mut self.browse, Browse::new(ui, offline));
        drop(keep);
        let g = ui.global::<GroveBrowse>();
        g.set_search_query("".into());
        g.set_search_tab(0);
        g.set_library_tab(0);
        g.set_library_filter("".into());
        g.set_library_note("".into());
        g.set_recent_searches(ModelRc::default());
        g.set_hero(GroveCard::default());
        g.set_header(GroveHeader::default());
        g.set_choice_row(-1);
        g.set_cooldown_visible(false);
    }
}

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let g = ui.global::<GroveBrowse>();
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_open(move |kind, uri, title| {
        if let Some(ui) = weak.upgrade() {
            if kind == "track" {
                ui.invoke_play_uri(uri);
                return;
            }
            f.borrow_mut().open_entry(&ui, &kind, &uri, &title, false);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_play(move |kind, uri| {
        if let Some(ui) = weak.upgrade() {
            match kind.as_str() {
                "album" | "playlist" if crate::spotify::spotify_uri(&uri).is_ok() => {
                    f.borrow_mut().browse.seeds.clear();
                    ui.invoke_play_uri(uri);
                }
                "track" => ui.invoke_play_uri(uri),
                _ => f.borrow_mut().open_entry(&ui, &kind, &uri, "", true),
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_row_action(move |action, index| {
        if let Some(ui) = weak.upgrade() {
            let play = f.borrow_mut().row_action(&ui, &action, index);
            if let Some(uri) = play {
                ui.invoke_play_uri(uri.into());
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_play_all(move |shuffle| {
        if let Some(ui) = weak.upgrade() {
            let context = {
                let mut f = f.borrow_mut();
                f.remember_seeds(None);
                let uri = f.browse.header.uri.clone();
                // A playlist whose songs Spotify does not share with Grove can
                // still play as a Spotify context.
                (f.rows.is_empty() && crate::spotify::spotify_uri(&uri).is_ok()).then_some(uri)
            };
            match context {
                Some(uri) => ui.invoke_play_uri(uri.into()),
                None => ui.invoke_play_selection(shuffle),
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_follow(move || {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().toggle_follow(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_retry(move || {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().retry(&ui);
        }
    });
    super::search::bind_browse(ui, this);
    super::library_tabs::bind(ui, this);
}

impl Features {
    /// Opens a collection, album, mix or artist page.
    pub(super) fn open_entry(
        &mut self,
        ui: &GroveWindow,
        kind: &str,
        uri: &str,
        title: &str,
        play: bool,
    ) {
        if !ui.get_library_ready() {
            ui.set_feature_message("Connect Spotify to browse your music.".into());
            return;
        }
        // Set first: a cached page is shown (and played) during the open itself.
        self.browse.play_on_load = play;
        match kind {
            "artist" => self.open_artist_page(ui, uri, title, vec![]),
            "album" | "playlist" | "liked" | "mix" => self.open_collection(ui, kind, uri, title),
            _ => {}
        }
        if !matches!(ui.get_page(), COLLECTION | ARTIST) {
            self.browse.play_on_load = false;
        }
    }

    pub(super) fn row_action(
        &mut self,
        ui: &GroveWindow,
        action: &str,
        index: i32,
    ) -> Option<String> {
        let i = usize::try_from(index).ok()?;
        let track = self.rows.get(i)?.clone();
        match action {
            "play" => {
                if ui.get_page() == SEARCH {
                    return Some(track.uri);
                }
                // Play from this row: the following rows become Grove's queue.
                if !ui.get_authenticated() || ui.get_jam_connected() {
                    return Some(track.uri);
                }
                self.remember_seeds(Some(i));
                self.queue = self
                    .rows
                    .iter()
                    .skip(i + 1)
                    .filter(|t| !self.prefs.hidden.contains(&t.uri))
                    .take(499)
                    .cloned()
                    .collect();
                self.render_queue(ui);
                Some(track.uri)
            }
            "Artists" => {
                let credits = self
                    .browse
                    .credits
                    .get(&track.uri)
                    .cloned()
                    .unwrap_or_default();
                ui.global::<GroveBrowse>().set_choice_row(index);
                self.set_choices(ui, &credits);
                None
            }
            "Artist" => {
                let credits = self
                    .browse
                    .credits
                    .get(&track.uri)
                    .cloned()
                    .unwrap_or_default();
                match credits.first() {
                    Some((name, uri)) => {
                        let others = credits[1..].to_vec();
                        self.open_artist_page(ui, &uri.clone(), &name.clone(), others);
                    }
                    None => self.open_artist(ui, Some(track)),
                }
                None
            }
            "Remove" => {
                self.browse.removal = Some(i);
                self.song(ui, "Remove", &track.uri);
                None
            }
            "Save" | "Unsave" => {
                if action == "Save" {
                    self.browse.saved.insert(track.uri.clone());
                } else {
                    self.browse.saved.remove(&track.uri);
                }
                self.song(ui, action, &track.uri);
                self.render(ui);
                None
            }
            other => {
                self.song(ui, other, &track.uri);
                None
            }
        }
    }

    pub(super) fn retry(&mut self, ui: &GroveWindow) {
        let Some(cooldown) = self.browse.cooldown.take() else {
            return;
        };
        ui.global::<GroveBrowse>().set_cooldown_visible(false);
        match cooldown.retry {
            Some(Retry::Page(action)) => {
                let action = super::navigation::first_page(action);
                self.load(ui, action, false);
            }
            Some(Retry::Tab(tab)) => self.library_tab(ui, tab, true),
            None => {}
        }
    }

    /// A 429 for the page or tab being browsed: show the cooldown banner.
    pub(super) fn show_cooldown(&mut self, ui: &GroveWindow, data: &Value, retry: Option<Retry>) {
        let seconds = data["seconds"].as_u64().unwrap_or(30).min(3600);
        let quota = data["quota"] == true;
        self.browse.cooldown = Some(Cooldown {
            until: Instant::now() + std::time::Duration::from_secs(seconds),
            quota,
            message: data["message"].as_str().unwrap_or_default().to_owned(),
            retry,
        });
        let g = ui.global::<GroveBrowse>();
        g.set_cooldown_visible(true);
        g.set_cooldown_quota(quota);
        g.set_cooldown_seconds(seconds as i32);
        g.set_cooldown_text(data["message"].as_str().unwrap_or_default().into());
    }

    pub(super) fn clear_cooldown(&mut self, ui: &GroveWindow) {
        if self.browse.cooldown.take().is_some() {
            ui.global::<GroveBrowse>().set_cooldown_visible(false);
        }
    }

    /// Runs every poll: cooldown countdown, batched lookups and autoplay refill.
    pub(super) fn browse_tick(&mut self, ui: &GroveWindow) {
        if let Some(cooldown) = &self.browse.cooldown {
            let left = cooldown
                .until
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                .ceil() as i32;
            let g = ui.global::<GroveBrowse>();
            if g.get_cooldown_seconds() != left {
                g.set_cooldown_seconds(left);
                g.set_cooldown_text(
                    if cooldown.quota || left <= 0 {
                        cooldown.message.clone()
                    } else {
                        crate::web_api::throttle_message(false, left as u64)
                    }
                    .into(),
                );
            }
        }
        self.flush_batch();
        self.autoplay_tick(ui);
    }

    /// Sends the TrackInfo lookups gathered this poll as one request.
    pub(super) fn flush_batch(&mut self) {
        let waiting: Vec<(u64, String)> = std::mem::take(&mut self.browse.batch)
            .into_iter()
            .filter(|(id, _)| self.pending.contains_key(id))
            .collect();
        match waiting.len() {
            0 => {}
            1 => {
                let (id, uri) = waiting[0].clone();
                if !self.offline {
                    let _ = self.tx.send(crate::spotify::Request::Feature {
                        id,
                        action: Action::TrackInfo { uri },
                    });
                }
            }
            _ => {
                let uris = waiting.iter().map(|(_, u)| u.clone()).collect();
                self.serial = self.serial.wrapping_add(1);
                let id = self.serial;
                self.pending
                    .insert(id, Pending::Browse(Want::Batch(waiting)));
                if self.offline {
                    self.browse.sent.push(Action::Tracks { uris });
                } else {
                    let _ = self.tx.send(crate::spotify::Request::Feature {
                        id,
                        action: Action::Tracks { uris },
                    });
                }
            }
        }
    }

    pub(super) fn batch_loaded(
        &mut self,
        ui: &GroveWindow,
        waiting: Vec<(u64, String)>,
        result: Result<Value, String>,
    ) {
        for (id, uri) in waiting {
            let reply = match &result {
                Ok(data) if data["kind"] == "throttled" => Ok(data.clone()),
                Ok(data) => data["tracks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|t| t["uri"].as_str() == Some(&uri))
                    .map(|t| serde_json::json!({"kind":"track_info","track":t}))
                    .ok_or_else(|| "Spotify couldn't find this song.".to_owned()),
                Err(e) => Err(e.clone()),
            };
            self.response(ui, id, reply);
        }
    }
}
