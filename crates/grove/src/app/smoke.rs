//! `--smoke-ui`: a scripted walk through the shell and every page that saves
//! native screenshots. Synthetic fixtures only: no Spotify, credentials,
//! network or preference writes. Lyrics come from the offline fixture.
use crate::{GroveFeatureTrack, GroveJamTrack, GrovePlaylist, GroveTrack, GroveWindow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::time::Duration;

type Stage = (&'static str, fn(&GroveWindow));

const WIDE: (f32, f32) = (1200.0, 800.0);
const NARROW: (f32, f32) = (700.0, 540.0);

fn size(ui: &GroveWindow, (w, h): (f32, f32)) {
    ui.window().set_size(slint::LogicalSize::new(w, h));
}

/// A colourful synthetic cover, so artwork colour and backdrops can be checked.
pub fn fixture_cover(seed: u8) -> slint::Image {
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(96, 96);
    for (i, p) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i % 96) as u32, (i / 96) as u32);
        let t = (x + y) as f32 / 190.0;
        let (a, b): ([f32; 3], [f32; 3]) = match seed % 4 {
            0 => ([236.0, 120.0, 60.0], [120.0, 40.0, 110.0]),
            1 => ([40.0, 110.0, 220.0], [20.0, 30.0, 70.0]),
            2 => ([60.0, 200.0, 140.0], [20.0, 70.0, 60.0]),
            _ => ([230.0, 200.0, 70.0], [150.0, 60.0, 40.0]),
        };
        // A soft centre label, like a record: smooth so scaling stays clean.
        let d = (((x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2)).sqrt() / 14.0).min(1.0);
        let ring = 0.6 + 0.4 * d * d;
        let c = |k: usize| ((a[k] + (b[k] - a[k]) * t) * ring) as u8;
        *p = slint::Rgba8Pixel {
            r: c(0),
            g: c(1),
            b: c(2),
            a: 255,
        };
    }
    slint::Image::from_rgba8(pixels)
}

fn track(title: &str, artist: &str, duration: &str, id: char) -> GroveTrack {
    GroveTrack {
        title: title.into(),
        artist: artist.into(),
        duration: duration.into(),
        uri: format!("spotify:track:{}", id.to_string().repeat(22)).into(),
    }
}

fn feature(title: &str, artist: &str, id: char, cover: u8) -> GroveFeatureTrack {
    GroveFeatureTrack {
        title: title.into(),
        artist: artist.into(),
        duration: "3:30".into(),
        uri: format!("spotify:track:{}", id.to_string().repeat(22)).into(),
        artist_uri: Default::default(),
        cover: fixture_cover(cover),
    }
}

fn signed_in(ui: &GroveWindow) {
    ui.set_library_ready(true);
    ui.set_account_name("Visual test library".into());
    ui.set_library_note("Synthetic fixtures · playback inactive".into());
    let names = [
        ("Evening rotation", "Synthetic playlist"),
        ("Forest walks", "Grove review"),
        ("Late focus", "Synthetic playlist"),
        ("Road trip, slow lane", "Grove review"),
        ("Rainy Sunday", "Synthetic playlist"),
        ("שירים לערב", "Grove review"),
    ];
    ui.set_playlists(ModelRc::new(VecModel::from(
        names
            .iter()
            .enumerate()
            .map(|(i, (title, subtitle))| GrovePlaylist {
                title: (*title).into(),
                subtitle: (*subtitle).into(),
                uri: format!(
                    "spotify:playlist:{}",
                    char::from(b'a' + i as u8).to_string().repeat(22)
                )
                .into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_playlist_covers(ModelRc::new(VecModel::from(
        (0..5u8).map(fixture_cover).collect::<Vec<_>>(),
    )));
}

fn playing(ui: &GroveWindow, title: &str, artist: &str) {
    ui.set_authenticated(true);
    ui.set_connected(true);
    ui.set_playing(true);
    ui.set_can_play(true);
    ui.set_can_pause(true);
    ui.set_can_previous(true);
    ui.set_can_next(true);
    ui.set_can_seek(true);
    ui.set_can_repeat(true);
    ui.set_can_shuffle(true);
    ui.set_current_uri("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into());
    ui.set_current_saved(true);
    ui.set_current_cover(fixture_cover(0));
    ui.set_current_album("Rings of the Grove".into());
    ui.set_context_name("Evening rotation".into());
    ui.set_track_title(title.into());
    ui.set_artist(artist.into());
    ui.set_status("Visual fixture".into());
    ui.set_position(22_000.0);
    ui.set_duration(210_000.0);
    ui.set_elapsed("0:22".into());
    ui.set_total("3:30".into());
    ui.set_volume(65.0);
}

fn queue(ui: &GroveWindow) {
    ui.set_local_queue(ModelRc::new(VecModel::from(vec![
        track("Morning light", "Grove review", "3:12", 'b'),
        track("Quiet chord", "Synthetic fixture", "4:05", 'c'),
        track("Roots and rivers", "Grove review", "2:58", 'd'),
    ])));
    ui.set_autoplay_queue(ModelRc::new(VecModel::from(vec![
        track("Soft and slow", "Autoplay fixture", "3:44", 'e'),
        track("End to end", "Autoplay fixture", "3:21", 'f'),
    ])));
    ui.set_tracks(ModelRc::new(VecModel::from(vec![track(
        "Spotify queue entry",
        "Read-only provider queue",
        "3:30",
        'g',
    )])));
    ui.set_autoplay(true);
}

const HEBREW: &str = "שיר ארוך מאוד על יער ירוק ותקליט שבור שמתנגן כל הלילה";
const ARABIC: &str = "فرقة الغابة الخضراء والأسطوانة المكسورة";

const STAGES: &[Stage] = &[
    ("welcome", |ui| {
        ui.set_page(5);
        size(ui, WIDE);
    }),
    ("welcome-narrow", |ui| size(ui, NARROW)),
    ("connection", |ui| {
        size(ui, WIDE);
        ui.set_page(2);
    }),
    ("connection-narrow", |ui| size(ui, NARROW)),
    ("library", |ui| {
        size(ui, WIDE);
        signed_in(ui);
        ui.set_page(3);
    }),
    ("liked-songs", |ui| {
        ui.invoke_browse("liked".into(), "Liked Songs".into());
        ui.set_feature_tracks(ModelRc::new(VecModel::from(vec![
            feature("Forest floor", "Grove review", 'a', 0),
            feature("Evening light", "Synthetic fixture", 'b', 1),
            feature(HEBREW, ARABIC, 'c', 2),
        ])));
        ui.set_feature_busy(false);
        ui.set_feature_message("".into());
    }),
    ("library-narrow", |ui| {
        ui.set_collection_uri("".into());
        size(ui, NARROW);
    }),
    // Back returns to Liked Songs, forward to the library list.
    ("", |ui| {
        assert!(
            ui.get_can_go_back(),
            "History did not record the library visits"
        );
        size(ui, WIDE);
        ui.invoke_navigate_back();
    }),
    ("history-back", |ui| {
        assert_eq!(ui.get_page(), 3);
        assert_eq!(
            ui.get_collection_uri().as_str(),
            "liked",
            "Back did not return to Liked Songs"
        );
        assert!(ui.get_can_go_forward());
    }),
    ("now-playing-lyrics", |ui| {
        playing(
            ui,
            "Native playback fixture",
            "Visual test · no Spotify session",
        );
        ui.set_now_playing_tab(0);
        ui.set_page(0);
    }),
    ("", |_| {}),
    ("now-playing-lyrics-active", |ui| {
        assert_eq!(
            ui.get_lyrics_state(),
            2,
            "Offline lyrics fixture did not load"
        );
        assert!(
            ui.get_now_playing_cover().size().width > 96,
            "Now Playing cover was not enlarged"
        );
        assert!(
            ui.get_now_playing_backdrop().size().width > 0,
            "No Now Playing backdrop"
        );
        assert!(
            ui.get_lyrics_active() >= 0,
            "No lyric line follows the position"
        );
        let line = ui.get_lyrics_active();
        ui.invoke_lyrics_seek(4);
        assert!(line >= 0);
    }),
    ("now-playing-accent", |ui| {
        ui.set_accent_follows_art(true);
    }),
    ("now-playing-light", |ui| {
        ui.set_light_mode(true);
    }),
    ("now-playing-narrow", |ui| {
        ui.set_light_mode(false);
        ui.set_accent_follows_art(false);
        size(ui, NARROW);
    }),
    ("now-playing-up-next", |ui| {
        size(ui, WIDE);
        queue(ui);
        ui.set_now_playing_tab(1);
    }),
    ("", |ui| {
        ui.set_now_playing_tab(0);
        ui.set_current_uri("spotify:track:hhhhhhhhhhhhhhhhhhhhhh".into());
        ui.set_track_title("No lyrics fixture".into());
        ui.set_current_cover(fixture_cover(1));
    }),
    ("now-playing-no-lyrics", |_| {}),
    ("queue-drawer", |ui| {
        assert_eq!(
            ui.get_lyrics_state(),
            3,
            "Missing lyrics did not show the empty state"
        );
        playing(
            ui,
            "Native playback fixture",
            "Visual test · no Spotify session",
        );
        ui.set_page(5);
        ui.set_queue_open(true);
    }),
    ("queue-drawer-light", |ui| ui.set_light_mode(true)),
    ("queue-drawer-narrow", |ui| {
        ui.set_light_mode(false);
        size(ui, NARROW);
    }),
    ("rtl-player", |ui| {
        ui.set_queue_open(false);
        size(ui, WIDE);
        ui.set_track_title(HEBREW.into());
        ui.set_artist(ARABIC.into());
        ui.set_page(3);
        ui.set_collection_uri("liked".into());
    }),
    ("rtl-player-narrow", |ui| size(ui, NARROW)),
    ("rtl-now-playing", |ui| {
        size(ui, WIDE);
        ui.set_page(0);
    }),
    ("hidden-current", |ui| {
        ui.set_page(5);
        ui.set_current_hidden(true);
    }),
    ("account-signed-in", |ui| {
        ui.set_current_hidden(false);
        ui.set_page(2);
    }),
    ("account-light", |ui| ui.set_light_mode(true)),
    ("compact", |ui| {
        ui.set_light_mode(false);
        ui.set_compact_mode(true);
        size(ui, (760.0, 640.0));
        ui.set_page(3);
    }),
    ("mini", |ui| {
        ui.set_compact_mode(false);
        ui.invoke_switch_mini();
    }),
    ("restored", |ui| {
        ui.invoke_switch_mini();
        size(ui, WIDE);
        ui.set_page(5);
    }),
    ("jam-setup", |ui| {
        ui.set_playing(false);
        ui.set_page(4);
    }),
    ("jam-host", |ui| {
        ui.set_jam_connected(true);
        ui.set_jam_host(true);
        ui.set_jam_invite("grove-jam://127.0.0.1:9876/visual-test-invitation-only".into());
        ui.set_jam_members("You (host) · Alice · Bob · Synthetic fixtures".into());
        ui.set_jam_sync_status(
            "Sharing host playback · listeners follow on their own devices".into(),
        );
        ui.set_jam_tracks(ModelRc::new(VecModel::from(vec![GroveJamTrack {
            id: "fixture".into(),
            title: "Evening song · synthetic fixture".into(),
            subtitle: "Added by Alice · Waiting".into(),
            removable: true,
        }])));
    }),
    ("jam-guest", |ui| {
        ui.set_jam_host(false);
        ui.set_jam_sync_status("Following Evening song · Playing · Synthetic fixture".into());
    }),
    ("jam-narrow", |ui| size(ui, NARROW)),
    ("minimum", |ui| {
        ui.set_jam_connected(false);
        ui.set_library_ready(false);
        ui.set_authenticated(false);
        ui.set_feature_tracks(ModelRc::default());
        ui.set_page(5);
    }),
    // Windows integration (#19-#21, #32, #33): Settings cards and the crash notice.
    ("settings", |ui| {
        size(ui, WIDE);
        ui.set_page(8);
        crate::app::system_fixtures(ui);
    }),
    ("settings-narrow", |ui| size(ui, NARROW)),
    ("settings-light", |ui| {
        size(ui, WIDE);
        ui.invoke_appearance("theme-system".into());
        crate::theme_watch::simulate(Some(true));
        crate::theme_watch::poll(ui);
    }),
    ("crash-notice", |ui| {
        crate::theme_watch::simulate(Some(false));
        crate::theme_watch::poll(ui);
        ui.set_page(5);
        let system = ui.global::<crate::GroveSystem>();
        system.set_crash_report(
            crate::crash::redact(
                "Grove crashed
message: synthetic fixture for listener@example.com
",
                &[],
                &Default::default(),
            )
            .into(),
        );
        system.set_crash_pending(true);
    }),
    ("crash-notice-narrow", |ui| size(ui, NARROW)),
];

/// Starts the screenshot script; `index` is the position of `--smoke-ui` in the arguments.
pub fn start(
    ui: &GroveWindow,
    smoke_timer: &slint::Timer,
    index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(index + 1)
        .ok_or("--smoke-ui needs an output directory")?;
    std::fs::create_dir_all(&output)?;
    // Screenshots show settled states; motion is checked by eye in the app.
    ui.global::<crate::GroveMotion>().set_enabled(false);
    let weak = ui.as_weak();
    let mut step = 0usize;
    smoke_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(450),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            // Capture what the previous stage set up, then set up the next.
            if let Some((name, _)) = step.checked_sub(1).and_then(|i| STAGES.get(i)) {
                if !name.is_empty() {
                    let pixels = ui.window().take_snapshot().expect("Native snapshot");
                    image::RgbaImage::from_raw(
                        pixels.width(),
                        pixels.height(),
                        pixels.as_bytes().to_vec(),
                    )
                    .unwrap()
                    .save(std::path::Path::new(&output).join(format!("{name}.png")))
                    .unwrap();
                }
            }
            match STAGES.get(step) {
                Some((_, setup)) => {
                    setup(&ui);
                    // Render once now so change handlers (such as the page
                    // fade) start immediately even if the window is occluded.
                    let _ = ui.window().take_snapshot();
                }
                None => {
                    eprintln!("PASS: shell smoke screenshots ({} stages)", STAGES.len());
                    slint::quit_event_loop().unwrap();
                }
            }
            step += 1;
        },
    );
    Ok(())
}
