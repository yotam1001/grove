//! `--smoke-ui`: a scripted walk through every page that saves native screenshots.
use crate::{GroveFeatureTrack, GroveJamTrack, GrovePlaylist, GroveTrack, GroveWindow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::time::Duration;

/// Starts the screenshot script; `index` is the position of `--smoke-ui` in the arguments.
pub fn start(
    ui: &GroveWindow,
    smoke_timer: &slint::Timer,
    index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    ui.set_page(0);
    let output = std::env::args()
        .nth(index + 1)
        .ok_or("--smoke-ui needs an output directory")?;
    std::fs::create_dir_all(&output)?;
    let weak = ui.as_weak();
    let mut step = 0;
    smoke_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if step == 0 {
                step += 1;
                return;
            }
            let name = match step {
                1 => "listen",
                2 => "connection",
                3 => "queue",
                4 => "compact",
                5 => "mini",
                6 => "restored",
                7 => "native-reference",
                8 => "library",
                9 => "liked-songs",
                10 => "connection-narrow",
                11 => "minimum",
                12 => "jam-setup",
                13 => "jam-host",
                14 => "jam-guest",
                15 => "jam-narrow",
                16 => "playing",
                17 => "playing-narrow",
                18 => "welcome",
                _ => "welcome-narrow",
            };
            let pixels = ui.window().take_snapshot().expect("Native snapshot");
            image::RgbaImage::from_raw(pixels.width(), pixels.height(), pixels.as_bytes().to_vec())
                .unwrap()
                .save(std::path::Path::new(&output).join(format!("{name}.png")))
                .unwrap();
            match step {
                1 => ui.set_page(2),
                2 => {
                    ui.set_page(1);
                    ui.set_tracks(ModelRc::new(VecModel::from(vec![GroveTrack {
                        title: "Native test track".into(),
                        artist: "Synthetic fixture · playback inactive".into(),
                        duration: "3:30".into(),
                        uri: String::new().into(),
                    }])));
                }
                3 => {
                    ui.set_page(0);
                    ui.set_tracks(ModelRc::default());
                    ui.window().set_size(slint::LogicalSize::new(760.0, 640.0));
                }
                4 => ui.invoke_switch_mini(),
                5 => {
                    ui.invoke_switch_mini();
                    ui.set_page(0);
                }
                6 => ui
                    .window()
                    .set_size(slint::LogicalSize::new(1536.0, 1024.0)),
                7 => {
                    ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
                    ui.set_page(3);
                    ui.set_library_ready(true);
                    ui.set_account_name("Visual test library · synthetic metadata".into());
                    ui.set_library_note("Synthetic fixtures · playback inactive".into());
                    ui.set_playlists(ModelRc::new(VecModel::from(vec![GrovePlaylist {
                        title: "Evening rotation".into(),
                        subtitle: "Synthetic playlist".into(),
                        uri: "spotify:playlist:2JRo0gjbX4GrCqBYdRohoo".into(),
                    }])));
                }
                8 => {
                    ui.set_collection_uri("liked".into());
                    ui.set_collection_title("Liked Songs".into());
                    ui.set_feature_tracks(ModelRc::new(VecModel::from(vec![GroveFeatureTrack {
                        title: "Native test track".into(),
                        artist: "Synthetic fixture · playback inactive".into(),
                        duration: "3:30".into(),
                        uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
                        artist_uri: Default::default(),
                        cover: Default::default(),
                    }])));
                }
                9 => {
                    ui.set_page(2);
                    ui.set_library_ready(false);
                    ui.set_account_name("Not signed in".into());
                    ui.window().set_size(slint::LogicalSize::new(700.0, 540.0));
                }
                10 => {
                    ui.set_page(0);
                }
                11 => {
                    ui.set_page(4);
                    ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
                }
                12 => {
                    ui.set_jam_connected(true);
                    ui.set_jam_host(true);
                    ui.set_jam_invite(
                        "grove-jam://127.0.0.1:9876/visual-test-invitation-only".into(),
                    );
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
                }
                13 => {
                    ui.set_jam_host(false);
                    ui.set_jam_sync_status(
                        "Following Evening song · Playing · Synthetic fixture".into(),
                    );
                }
                14 => ui.window().set_size(slint::LogicalSize::new(700.0, 540.0)),
                15 => {
                    ui.set_page(0);
                    ui.set_jam_connected(false);
                    ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
                    ui.set_authenticated(true);
                    ui.set_playing(true);
                    ui.set_can_pause(true);
                    ui.set_can_previous(true);
                    ui.set_can_next(true);
                    ui.set_can_seek(true);
                    ui.set_can_repeat(true);
                    ui.set_repeat(true);
                    ui.set_library_ready(true);
                    ui.set_current_uri("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into());
                    ui.set_current_saved(true);
                    ui.set_track_title("Native playback fixture".into());
                    ui.set_artist("Visual test · no Spotify session".into());
                    ui.set_status("Visual fixture".into());
                    ui.set_position(45000.0);
                    ui.set_duration(210000.0);
                    ui.set_elapsed("0:45".into());
                    ui.set_total("3:30".into());
                    ui.set_volume(65.0);
                }
                16 => ui.window().set_size(slint::LogicalSize::new(700.0, 540.0)),
                17 => {
                    ui.set_page(5);
                    ui.set_library_ready(false);
                    ui.set_authenticated(false);
                    ui.set_feature_tracks(ModelRc::default());
                    ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
                }
                18 => ui.window().set_size(slint::LogicalSize::new(700.0, 540.0)),
                _ => {
                    slint::quit_event_loop().unwrap();
                }
            }
            step += 1;
        },
    );
    Ok(())
}
