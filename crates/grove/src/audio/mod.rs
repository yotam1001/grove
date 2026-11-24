//! Grove's audio engine extensions: stream quality and normalisation, output
//! device choice and recovery, fades between songs, equalizer, sleep timer,
//! resume positions and session reconnects.
//!
//! The pure policies (`dsp`, `fade`, `sleep`, `reconnect`, `resume`,
//! `recovery`, `quality`, `settings`) carry the unit tests. `sink` runs on
//! librespot's player thread, `ui` binds Settings → Audio and Now playing,
//! and `review` drives those controls offline for screenshots.
pub mod control;
pub mod dsp;
pub mod fade;
pub mod quality;
pub mod reconnect;
pub mod recovery;
pub mod resume;
pub mod settings;
pub mod sink;
pub mod sleep;
