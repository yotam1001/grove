//! Fade between songs. librespot plays one stream and already preloads the
//! next song, so Grove fades the end of a song out and, after a natural
//! track change, the start of the next one in. The sink applies the gain.
//! Albums keep their gapless joins: no fade inside an album played in order.
//!
//! Grove's own queue (the drawer, autoplay refill) starts the next song with a
//! new load after librespot's end-of-track, so "natural" means a track change
//! shortly after an end, and the queue head is looked up ahead of time so an
//! album run queued from a browse page also keeps its joins.
use std::time::{Duration, Instant};

/// A load this soon after an end-of-track continues the listening session
/// (Grove's queue or autoplay picked it); later loads are the listener's.
pub const NATURAL_WINDOW: Duration = Duration::from_secs(5);

/// Where the sink is in the current song and which fades apply to it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackClock {
    /// Bumped on every discontinuity (new song, seek, play) so the sink
    /// restarts its sample count from `anchor_ms`.
    pub epoch: u64,
    pub anchor_ms: f64,
    pub duration_ms: f64,
    pub fade_ms: f64,
    pub fade_in: bool,
    pub fade_out: bool,
    /// Long enough to fade at all; Grove's queue lookahead may switch the
    /// fade-out on or off within this.
    pub eligible: bool,
}

/// Album position used to recognise a gapless join.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumSlot {
    pub album: String,
    pub disc: u32,
    pub number: u32,
}

/// Songs played from an album in order keep their gapless joins.
pub fn gapless_context(context_uri: &str, shuffle: bool) -> bool {
    context_uri.starts_with("spotify:album:") && !shuffle
}

/// `next` directly follows `previous` on the same album.
pub fn consecutive(previous: Option<&AlbumSlot>, next: Option<&AlbumSlot>) -> bool {
    match (previous, next) {
        (Some(a), Some(b)) if !a.album.is_empty() && a.album == b.album => {
            (a.disc == b.disc && b.number == a.number + 1)
                || (b.disc == a.disc + 1 && b.number == 1)
        }
        _ => false,
    }
}

/// Whether a song that starts `now` follows an end at `ended` on its own.
pub fn natural(ended: Option<Instant>, now: Instant) -> bool {
    ended.is_some_and(|at| now.saturating_duration_since(at) <= NATURAL_WINDOW)
}

/// The fade-out to use once Grove knows its next queued song: `Some(true)`
/// when that song continues the album gaplessly, `None` when the next song
/// is up to the Spotify context.
pub fn fade_out(clock: &TrackClock, next_gapless: Option<bool>) -> bool {
    clock.eligible && next_gapless.map_or(clock.fade_out, |gapless| !gapless)
}

/// Decide the fades for a song that just started.
/// `natural` is true when the previous song reached its end on its own.
pub fn plan(
    fade_seconds: u8,
    natural: bool,
    gapless: bool,
    joined: bool,
    duration_ms: f64,
) -> (bool, bool) {
    let fade_ms = f64::from(fade_seconds) * 1000.0;
    if fade_seconds == 0 || !duration_ms.is_finite() || duration_ms < 3.0 * fade_ms {
        return (false, false);
    }
    (natural && !gapless && !joined, !gapless)
}

/// Linear gain 0..=1 at `position_ms`. Equal-power curves, so the overall
/// level dips less than a straight line would.
pub fn gain(clock: &TrackClock, position_ms: f64) -> f64 {
    if clock.fade_ms <= 0.0 || !position_ms.is_finite() || clock.duration_ms <= 0.0 {
        return 1.0;
    }
    let half_pi = std::f64::consts::FRAC_PI_2;
    let mut gain: f64 = 1.0;
    if clock.fade_in && position_ms < clock.fade_ms {
        gain = gain.min((position_ms.max(0.0) / clock.fade_ms * half_pi).sin());
    }
    let left = clock.duration_ms - position_ms;
    if clock.fade_out && left < clock.fade_ms {
        gain = gain.min((left.max(0.0) / clock.fade_ms * half_pi).sin());
    }
    gain.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(fade_in: bool, fade_out: bool) -> TrackClock {
        TrackClock {
            epoch: 1,
            anchor_ms: 0.0,
            duration_ms: 200_000.0,
            fade_ms: 6_000.0,
            fade_in,
            fade_out,
            eligible: true,
        }
    }

    #[test]
    fn fades_shape_both_ends_with_equal_power() {
        let c = clock(true, true);
        assert_eq!(gain(&c, 0.0), 0.0);
        assert!((gain(&c, 3_000.0) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
        assert_eq!(gain(&c, 6_000.0), 1.0);
        assert_eq!(gain(&c, 100_000.0), 1.0);
        assert!((gain(&c, 197_000.0) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
        assert_eq!(gain(&c, 200_000.0), 0.0);
        assert_eq!(gain(&c, 250_000.0), 0.0, "Past the end stays silent");
        assert_eq!(
            gain(&clock(false, true), 0.0),
            1.0,
            "No fade-in after a skip"
        );
        assert_eq!(gain(&clock(true, false), 199_000.0), 1.0);
        assert_eq!(gain(&TrackClock::default(), 0.0), 1.0, "Off is unity");
        assert_eq!(gain(&c, f64::NAN), 1.0);
    }

    #[test]
    fn plan_skips_manual_starts_short_songs_and_gapless_albums() {
        assert_eq!(plan(0, true, false, false, 200_000.0), (false, false));
        assert_eq!(plan(6, true, false, false, 200_000.0), (true, true));
        assert_eq!(
            plan(6, false, false, false, 200_000.0),
            (false, true),
            "Skips start at full level"
        );
        assert_eq!(
            plan(6, true, true, false, 200_000.0),
            (false, false),
            "Album in order"
        );
        assert_eq!(
            plan(6, true, false, true, 200_000.0),
            (false, true),
            "Consecutive album tracks"
        );
        assert_eq!(
            plan(12, true, false, false, 30_000.0),
            (false, false),
            "Too short to fade"
        );
        assert_eq!(plan(6, true, false, false, f64::INFINITY), (false, false));
    }

    #[test]
    fn grove_queue_and_autoplay_count_as_natural_but_late_picks_do_not() {
        let end = Instant::now();
        assert!(
            natural(Some(end), end + Duration::from_millis(300)),
            "Queue head after EOF"
        );
        assert!(natural(Some(end), end + NATURAL_WINDOW));
        assert!(
            !natural(Some(end), end + NATURAL_WINDOW + Duration::from_millis(1)),
            "A later manual pick starts at full level"
        );
        assert!(!natural(None, end), "A skip or drawer pick has no end");
    }

    #[test]
    fn the_queue_lookahead_overrides_the_context_guess() {
        let mut c = clock(false, true);
        c.eligible = true;
        assert!(fade_out(&c, None), "Unknown next: keep the planned fade");
        assert!(
            !fade_out(&c, Some(true)),
            "Next queued song continues the album"
        );
        assert!(fade_out(&c, Some(false)));
        c.fade_out = false;
        assert!(
            fade_out(&c, Some(false)),
            "Album context, but the queue plays something else next"
        );
        c.eligible = false;
        assert!(!fade_out(&c, Some(false)), "Too short or fades off");
    }

    #[test]
    fn album_order_and_consecutive_tracks_are_gapless() {
        assert!(gapless_context(
            "spotify:album:2JRo0gjbX4GrCqBYdRohoo",
            false
        ));
        assert!(!gapless_context(
            "spotify:album:2JRo0gjbX4GrCqBYdRohoo",
            true
        ));
        assert!(!gapless_context(
            "spotify:playlist:2JRo0gjbX4GrCqBYdRohoo",
            false
        ));
        let slot = |album: &str, disc, number| AlbumSlot {
            album: album.into(),
            disc,
            number,
        };
        assert!(consecutive(Some(&slot("A", 1, 4)), Some(&slot("A", 1, 5))));
        assert!(consecutive(Some(&slot("A", 1, 12)), Some(&slot("A", 2, 1))));
        assert!(!consecutive(Some(&slot("A", 1, 4)), Some(&slot("A", 1, 7))));
        assert!(!consecutive(Some(&slot("A", 1, 4)), Some(&slot("B", 1, 5))));
        assert!(!consecutive(Some(&slot("", 1, 4)), Some(&slot("", 1, 5))));
        assert!(!consecutive(None, Some(&slot("A", 1, 1))));
    }
}
