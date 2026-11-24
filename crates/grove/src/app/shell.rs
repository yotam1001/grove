//! The app shell (#41): back/forward history, sidebar playlist artwork and the
//! colour Grove derives from the current cover (#13).
use crate::GroveWindow;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

thread_local! {
    /// Playlist artwork URLs, parallel to the window's `playlists` model.
    static PLAYLIST_ART: RefCell<(u64, Vec<String>)> = const { RefCell::new((0, Vec::new())) };
}

/// The sidebar shows artwork for at most this many playlists; the artwork
/// cache is bounded, and later rows use the placeholder.
const SIDEBAR_COVERS: usize = 40;

/// Records playlist artwork from a Spotify playlists page (`items[].images`).
pub fn set_playlist_artwork(playlists: &serde_json::Value) {
    let urls = playlists["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"].is_string() && p["uri"].is_string())
        .map(|p| {
            p["images"][0]["url"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .take(SIDEBAR_COVERS)
        .collect();
    PLAYLIST_ART.with(|art| {
        let mut art = art.borrow_mut();
        art.0 += 1;
        art.1 = urls;
    });
}

/// A place in the back/forward history.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub page: i32,
    pub collection: String,
    pub title: String,
}

/// Back/forward history. Artist and radio pages (7) are transient: their rows
/// are not kept, so they are left out of the stacks.
#[derive(Default)]
pub struct History {
    back: Vec<Place>,
    forward: Vec<Place>,
    current: Option<Place>,
    restoring: Option<Place>,
}

impl History {
    const LIMIT: usize = 50;
    /// Notes where the window is now. A new place clears forward history.
    pub fn observe(&mut self, place: Place) {
        if self.current.as_ref() == Some(&place) {
            return;
        }
        if self.restoring.take().as_ref() == Some(&place) {
            self.current = Some(place);
            return;
        }
        if let Some(previous) = self.current.take().filter(|p| p.page != 7) {
            if self.back.last() != Some(&previous) {
                self.back.push(previous);
            }
            if self.back.len() > Self::LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
        self.current = Some(place);
    }
    pub fn back(&mut self) -> Option<Place> {
        let target = self.back.pop()?;
        if let Some(current) = self.current.take().filter(|p| p.page != 7) {
            self.forward.push(current);
        }
        self.restoring = Some(target.clone());
        Some(target)
    }
    pub fn forward(&mut self) -> Option<Place> {
        let target = self.forward.pop()?;
        if let Some(current) = self.current.take().filter(|p| p.page != 7) {
            self.back.push(current);
        }
        self.restoring = Some(target.clone());
        Some(target)
    }
    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

fn place(ui: &GroveWindow) -> Place {
    let page = ui.get_page();
    let (collection, title) = if page == 3 && !ui.get_collection_uri().is_empty() {
        (
            ui.get_collection_uri().into(),
            ui.get_collection_title().into(),
        )
    } else if page == 7 {
        (String::new(), ui.get_feature_title().into())
    } else {
        (String::new(), String::new())
    };
    Place {
        page,
        collection,
        title,
    }
}

fn go(ui: &GroveWindow, target: &Place) {
    if target.page == 3 && !target.collection.is_empty() {
        if ui.get_page() == 3 {
            ui.invoke_browse(
                target.collection.clone().into(),
                target.title.clone().into(),
            );
        } else {
            ui.set_collection_uri(target.collection.clone().into());
            ui.set_collection_title(target.title.clone().into());
            ui.set_collection_owned(
                ui.get_owned_playlists()
                    .iter()
                    .any(|p| p.uri.as_str() == target.collection),
            );
            ui.set_page(3);
        }
    } else {
        if target.page == 3 {
            ui.set_collection_uri("".into());
            ui.set_collection_title("".into());
        }
        ui.set_page(target.page);
    }
}

/// A representative colour for a cover, favouring colourful pixels. `None`
/// for grey or missing artwork, so the accent stays Grove green.
pub fn artwork_tint(image: &slint::Image) -> Option<[u8; 3]> {
    let pixels = image.to_rgba8()?;
    let (mut red, mut green, mut blue, mut weight) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for pixel in pixels.as_slice().iter().step_by(7) {
        let (r, g, b) = (f32::from(pixel.r), f32::from(pixel.g), f32::from(pixel.b));
        let w = 0.05 + (r.max(g).max(b) - r.min(g).min(b)) / 255.0;
        red += r * w;
        green += g * w;
        blue += b * w;
        weight += w;
    }
    if weight <= 0.0 {
        return None;
    }
    let (r, g, b) = (red / weight, green / weight, blue / weight);
    if r.max(g).max(b) - r.min(g).min(b) < 24.0 {
        return None;
    }
    Some([r as u8, g as u8, b as u8])
}

fn linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
pub fn luminance(c: [u8; 3]) -> f32 {
    0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2])
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn mix(c: [u8; 3], toward: [u8; 3], amount: f32) -> [u8; 3] {
    let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    [m(c[0], toward[0]), m(c[1], toward[1]), m(c[2], toward[2])]
}

/// Dark-theme canvas, dark ink on accent buttons, and the light-theme canvas.
const DARK_CANVAS: [u8; 3] = [0x12, 0x14, 0x15];
const DARK_ON_ACCENT: [u8; 3] = [0x08, 0x17, 0x0f];
const LIGHT_CANVAS: [u8; 3] = [0xf3, 0xf6, 0xf3];

/// Accent variants of a tint that keep at least 4.5:1 contrast: against the
/// dark canvas and the dark ink on accent buttons, and against white text and
/// light surfaces.
pub fn contrast_safe_accents(tint: [u8; 3]) -> ([u8; 3], [u8; 3]) {
    let mut dark = tint;
    let mut step = 0.0;
    while (contrast(dark, DARK_CANVAS) < 4.5 || contrast(dark, DARK_ON_ACCENT) < 4.5) && step < 1.0
    {
        step += 0.05;
        dark = mix(tint, [255, 255, 255], step);
    }
    let mut light = tint;
    let mut step = 0.0;
    while (contrast(light, [255, 255, 255]) < 4.5 || contrast(light, LIGHT_CANVAS) < 4.5)
        && step < 1.0
    {
        step += 0.05;
        light = mix(tint, [0, 0, 0], step);
    }
    (dark, light)
}

/// A soft, blurred copy of the cover for Now Playing's drifting backdrop.
/// Blurs a small bitmap once per cover rather than the window per frame.
pub fn backdrop(source: &slint::Image) -> slint::Image {
    let size = source.size();
    if size.width == 0 || size.height == 0 || size.width > 512 || size.height > 512 {
        return slint::Image::default();
    }
    let Some(pixels) = source.to_rgba8() else {
        return slint::Image::default();
    };
    let Some(original) =
        image::RgbaImage::from_raw(size.width, size.height, pixels.as_bytes().to_vec())
    else {
        return slint::Image::default();
    };
    let small = image::imageops::resize(&original, 48, 48, image::imageops::FilterType::Triangle);
    let blurred = image::imageops::blur(&small, 4.0);
    let side = 192;
    let large =
        image::imageops::resize(&blurred, side, side, image::imageops::FilterType::Triangle);
    slint::Image::from_rgba8(
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(large.as_raw(), side, side),
    )
}

/// The 96px thumbnail resampled smoothly for Now Playing's large cover, so
/// the software renderer does not show blocky pixels.
pub fn enlarge(source: &slint::Image) -> slint::Image {
    let size = source.size();
    if size.width == 0 || size.height == 0 || size.width > 512 || size.height > 512 {
        return slint::Image::default();
    }
    let Some(pixels) = source.to_rgba8() else {
        return slint::Image::default();
    };
    let Some(original) =
        image::RgbaImage::from_raw(size.width, size.height, pixels.as_bytes().to_vec())
    else {
        return slint::Image::default();
    };
    let side = 384;
    let large = image::imageops::resize(
        &original,
        side,
        side,
        image::imageops::FilterType::CatmullRom,
    );
    slint::Image::from_rgba8(
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(large.as_raw(), side, side),
    )
}

fn color(c: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(c[0], c[1], c[2])
}

struct ShellState {
    history: History,
    covers: crate::artwork::Cache,
    cover_generation: u64,
    cover_urls: Vec<String>,
    art_source: Option<slint::Image>,
    backdrop_source: Option<slint::Image>,
}

/// Owns the shell's timer; drop it to stop.
pub struct Shell {
    timer: slint::Timer,
}
impl Drop for Shell {
    fn drop(&mut self) {
        self.timer.stop();
    }
}

pub fn install(ui: &GroveWindow, offline: bool) -> Shell {
    let state = Rc::new(RefCell::new(ShellState {
        history: History::default(),
        covers: crate::artwork::Cache::new(offline),
        cover_generation: 0,
        cover_urls: Vec::new(),
        art_source: None,
        backdrop_source: None,
    }));
    {
        let state = state.clone();
        let weak = ui.as_weak();
        ui.on_navigate_back(move || {
            let Some(ui) = weak.upgrade() else { return };
            let target = {
                let mut s = state.borrow_mut();
                s.history.observe(place(&ui));
                s.history.back()
            };
            if let Some(target) = target {
                // An artist page restores the list it was opened from.
                if ui.get_page() == 7 {
                    ui.invoke_feature_back();
                }
                if place(&ui) != target {
                    go(&ui, &target);
                }
            }
            sync_history(&ui, &state.borrow().history);
        });
    }
    {
        let state = state.clone();
        let weak = ui.as_weak();
        ui.on_navigate_forward(move || {
            let Some(ui) = weak.upgrade() else { return };
            let target = {
                let mut s = state.borrow_mut();
                s.history.observe(place(&ui));
                s.history.forward()
            };
            if let Some(target) = target {
                go(&ui, &target);
            }
            sync_history(&ui, &state.borrow().history);
        });
    }
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(120),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            // The page fade's own one-frame timer can wait for a redraw; never
            // leave a page transparent for longer than one shell tick.
            if ui.get_page_entering() {
                ui.set_page_entering(false);
            }
            let mut s = state.borrow_mut();
            s.history.observe(place(&ui));
            sync_history(&ui, &s.history);
            update_covers(&ui, &mut s);
            update_artwork(&ui, &mut s);
        },
    );
    Shell { timer }
}

fn sync_history(ui: &GroveWindow, history: &History) {
    if ui.get_can_go_back() != history.can_back() {
        ui.set_can_go_back(history.can_back());
    }
    if ui.get_can_go_forward() != history.can_forward() {
        ui.set_can_go_forward(history.can_forward());
    }
}

fn update_covers(ui: &GroveWindow, s: &mut ShellState) {
    let (generation, urls) = PLAYLIST_ART.with(|art| {
        let art = art.borrow();
        (art.0, (art.0 != s.cover_generation).then(|| art.1.clone()))
    });
    let fresh = urls.is_some();
    if let Some(urls) = urls {
        s.cover_generation = generation;
        s.cover_urls = urls;
    }
    if (s.covers.poll() || fresh) && !s.cover_urls.is_empty() {
        let covers: Vec<_> = s.cover_urls.iter().map(|u| s.covers.get(u)).collect();
        ui.set_playlist_covers(ModelRc::new(VecModel::from(covers)));
    }
}

fn update_artwork(ui: &GroveWindow, s: &mut ShellState) {
    let cover = if ui.get_local_active() {
        slint::Image::default()
    } else {
        ui.get_current_cover()
    };
    if s.art_source.as_ref() != Some(&cover) {
        let theme = ui.global::<crate::GroveTheme>();
        match artwork_tint(&cover) {
            Some(tint) => {
                let (dark, light) = contrast_safe_accents(tint);
                theme.set_art_tint(color(tint));
                theme.set_art_accent_dark(color(dark));
                theme.set_art_accent_light(color(light));
                theme.set_art_valid(true);
            }
            None => theme.set_art_valid(false),
        }
        s.art_source = Some(cover.clone());
    }
    // The backdrop and the smooth large cover exist only while Now Playing
    // is on screen.
    let wanted = ui.get_now_playing_visible().then_some(cover);
    if wanted != s.backdrop_source {
        ui.set_now_playing_backdrop(wanted.as_ref().map(backdrop).unwrap_or_default());
        ui.set_now_playing_cover(wanted.as_ref().map(enlarge).unwrap_or_default());
        s.backdrop_source = wanted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(page: i32) -> Place {
        Place {
            page,
            collection: String::new(),
            title: String::new(),
        }
    }

    #[test]
    fn back_and_forward_walk_visited_places() {
        let mut h = History::default();
        h.observe(at(5));
        h.observe(at(6));
        let playlist = Place {
            page: 3,
            collection: "spotify:playlist:x".into(),
            title: "Mix".into(),
        };
        h.observe(playlist.clone());
        assert!(h.can_back() && !h.can_forward());
        assert_eq!(h.back(), Some(at(6)));
        h.observe(at(6));
        assert_eq!(h.back(), Some(at(5)));
        h.observe(at(5));
        assert!(!h.can_back());
        assert_eq!(h.forward(), Some(at(6)));
        h.observe(at(6));
        assert_eq!(h.forward(), Some(playlist.clone()));
        h.observe(playlist);
        // A new place clears forward history.
        h.back();
        h.observe(at(6));
        h.observe(at(4));
        assert!(!h.can_forward());
    }

    #[test]
    fn artist_pages_are_not_kept_in_history() {
        let mut h = History::default();
        h.observe(at(5));
        h.observe(at(7));
        h.observe(at(6));
        assert_eq!(h.back(), Some(at(5)));
        h.observe(at(5));
        assert_eq!(h.forward(), Some(at(6)));
    }

    #[test]
    fn artwork_accents_keep_contrast_in_both_themes() {
        for tint in [
            [30, 40, 200],
            [200, 30, 40],
            [20, 120, 60],
            [250, 240, 40],
            [90, 20, 110],
            [53, 223, 130],
        ] {
            let (dark, light) = contrast_safe_accents(tint);
            assert!(contrast(dark, DARK_CANVAS) >= 4.5, "{tint:?} -> {dark:?}");
            assert!(
                contrast(dark, DARK_ON_ACCENT) >= 4.5,
                "{tint:?} -> {dark:?}"
            );
            assert!(
                contrast(light, [255, 255, 255]) >= 4.5,
                "{tint:?} -> {light:?}"
            );
            assert!(
                contrast(light, LIGHT_CANVAS) >= 4.5,
                "{tint:?} -> {light:?}"
            );
        }
        assert!((contrast([0, 0, 0], [255, 255, 255]) - 21.0).abs() < 0.01);
    }

    #[test]
    fn grey_or_missing_covers_keep_the_grove_accent() {
        assert_eq!(artwork_tint(&slint::Image::default()), None);
        let mut grey = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(8, 8);
        for p in grey.make_mut_slice() {
            *p = slint::Rgba8Pixel {
                r: 90,
                g: 92,
                b: 91,
                a: 255,
            };
        }
        assert_eq!(artwork_tint(&slint::Image::from_rgba8(grey)), None);
        let mut blue = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(8, 8);
        for p in blue.make_mut_slice() {
            *p = slint::Rgba8Pixel {
                r: 20,
                g: 40,
                b: 200,
                a: 255,
            };
        }
        let tint = artwork_tint(&slint::Image::from_rgba8(blue)).unwrap();
        assert!(tint[2] > tint[0]);
        assert_eq!(backdrop(&slint::Image::default()).size().width, 0);
    }
}
