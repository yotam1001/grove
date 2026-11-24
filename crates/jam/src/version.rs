//! Protocol versions shared by Tide and Grove, and the messages shown on mismatch.
//!
//! * Nearby rooms (`grove-jam://`): encrypted TCP, [`LAN_PROTOCOL`].
//! * Relay rooms: Tide's HTTP relay. Protocol [`RELAY_TIDE`] is Tide's current
//!   YouTube-only timeline (`x-tide-jam-protocol: 1`, `tide-jam:v1.` invites).
//!   Protocol [`RELAY_CROSS`] is the cross-service extension (`grove-jam:v2.`
//!   invites) served by `grove-jam-relay`; Tide needs the same change to host
//!   or follow those rooms.

/// Nearby room protocol. v2 was the unencrypted JSON-over-TCP prototype.
pub const LAN_PROTOCOL: u16 = 3;
/// Oldest nearby protocol this build can talk to.
pub const LAN_MIN_PROTOCOL: u16 = 3;
/// Tide's relay protocol (experiments/jam/src/client.rs header value).
pub const RELAY_TIDE: u8 = 1;
/// Cross-service relay protocol: provider URIs plus title/artist/duration/ISRC.
pub const RELAY_CROSS: u8 = 2;
/// Tide commit whose jam protocol this crate mirrors and tests against.
/// Keep in step with the `tide-jam-lab` dev-dependency `rev` in Cargo.toml.
pub const TIDE_JAM_REV: &str = "4bc2ba3ddfa8baa68fb775cbafc0a1df6735cba8";

/// The app on the other side, as announced in the nearby handshake.
pub fn app_name(app: &str) -> &'static str {
    match app {
        "tide" => "Tide",
        "grove" => "Grove",
        _ => "Tide or Grove",
    }
}

/// Guest-side message when the host speaks another nearby protocol.
pub fn host_mismatch(host_app: &str, host_protocol: u16, host_min: u16) -> String {
    let app = app_name(host_app);
    if host_min > LAN_PROTOCOL {
        format!("This jam needs a newer app (jam protocol {host_protocol}). Update Grove to join.")
    } else {
        format!(
            "The host's {app} uses an older jam protocol ({host_protocol}). Ask them to update {app}."
        )
    }
}

/// Host-side reply when an older or newer guest tries to join.
pub fn guest_mismatch(guest_protocol: u16) -> String {
    if guest_protocol < LAN_MIN_PROTOCOL {
        format!(
            "This jam needs jam protocol {LAN_PROTOCOL}; your app has {guest_protocol}. Update Tide or Grove to join."
        )
    } else {
        format!(
            "This jam's host runs an older app (jam protocol {LAN_PROTOCOL}). Ask the host to update Tide or Grove."
        )
    }
}

/// Shown when an invite or relay announces a protocol this build does not know.
pub fn relay_mismatch(required: u8) -> String {
    if required > RELAY_CROSS {
        format!("This jam uses relay protocol {required}. Update Grove to join.")
    } else if required == RELAY_CROSS {
        "This relay only runs Tide's protocol 1. Grove can join Tide rooms there; to host a Grove room, use a relay updated for protocol 2 (grove-jam-relay).".into()
    } else {
        "This room was made by a newer cross-service relay. Update Tide or Grove to join.".into()
    }
}

/// Pick the common nearby protocol, or explain why there is none.
pub fn negotiate(peer_protocol: u16, peer_min: u16) -> Option<u16> {
    let best = peer_protocol.min(LAN_PROTOCOL);
    if best >= peer_min.max(LAN_MIN_PROTOCOL) {
        Some(best)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mismatch_messages_name_the_app_to_update() {
        assert_eq!(negotiate(3, 3), Some(3));
        assert_eq!(negotiate(4, 3), Some(3));
        assert!(negotiate(2, 2).is_none());
        assert!(negotiate(5, 4).is_none());
        assert!(host_mismatch("tide", 2, 2).contains("update Tide"));
        assert!(host_mismatch("grove", 5, 4).contains("Update Grove"));
        assert!(guest_mismatch(2).contains("Update Tide or Grove"));
        assert!(relay_mismatch(RELAY_CROSS).contains("protocol 2"));
        assert!(relay_mismatch(9).contains("Update Grove"));
    }
}
