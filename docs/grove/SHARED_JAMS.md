# Shared jams

Grove listeners can share nearby rooms (encrypted, on a private network) or
internet rooms through a self-hostable relay, and can join Tide's relay rooms.
See [Tide + Grove jams](../CROSS_SERVICE_JAMS.md) for protocols, encryption,
relay hosting, sync and matching details.

Grove's shared jams prioritize everyone listening on their own device. The host
shares track, play/pause and position updates; each guest uses their own Spotify
Premium playback approval to follow. Audio and Spotify credentials never pass
through the room connection. These are Grove rooms, not official Spotify Jam
rooms or invitations.

## Try it

1. On each computer, launch Grove and complete both approvals on **Account**
   using that listener's own account. Spotify generally permits one active stream
   per account; use separate accounts for independent listeners.
2. On the host, open **Shared jams**, enter a display name, choose **Nearby**
   (or **Internet** with your relay's address), then **Start a jam**. Grove
   selects a private local address when available.
   **Network options** allows a manual address override. A warning appears if
   the selected address is `127.0.0.1`, which only works on the same computer.
3. Copy the invitation (or its `grove://` link) and paste or open it on each
   guest's computer. Enter a display name and choose **Join jam**. Tide relay
   invites and four-letter codes work here too.
4. Play a Spotify track on the host. Guests follow automatically. Volume remains
   independent; **Stop following** gives a guest independent playback and
   **Follow host** resumes synchronization.
5. Everyone can add Spotify song, album or playlist links; Grove looks up the
   song names. Network options also exposes an optional manual name. The next
   waiting song starts automatically when the current one ends, or the host
   uses **Play next for everyone**. Guests can
   remove their own contributions; the host can remove any waiting or
   unconfirmed entry. Leaving preserves contributed tracks for the remaining room.

The host needs to remain open and reachable. Closing or ending the room disconnects
guests and pauses guests who were following. A dropped connection reconnects
automatically; if the host vanishes, a stand-in listener takes over. No firewall settings are changed automatically; Windows may require
allowing the app on your private network.

## Synchronization behavior

The host publishes a playback sample once per second. Nearby guests measure the
host's clock continuously (NTP-style pings every 2 s, lowest round trip of the
last eight) and project the host's position with it; relay guests use the
relay's sample age plus the measured round trip. Drift beyond 300 ms triggers a
seek once the clock is trusted (1.5 s before that), with a three-second
cooldown. Shared-queue songs start together: the host loads paused, guests load
and acknowledge, and everyone presses play at a scheduled instant. Samples older
than five seconds pause following guests until updates return. Three failed
loads stop automatic loading until the host changes tracks or the guest restarts
following.

This is still approximate synchronization, not sample-accurate audio. When the
host's song ends, the next waiting shared song starts automatically (switchable).
Listeners can vote to skip; a majority skips. If the host disappears from a
nearby room, a stand-in listener takes over and everyone reconnects; dropped
listeners rejoin automatically within about 25 seconds.

The shared queue is separate from Spotify's queue. An entry is removed only after
the host engine confirms playback of that track. If confirmation is missing, it
is marked unconfirmed and is not retried automatically. Check the host player
before removing an unconfirmed entry and moving on.

## Transport and limits

Nearby rooms live only in memory on the host and use encrypted, bounded JSON
frames over TCP on an ephemeral port, keyed from the invitation token (which
never crosses the network). Hosting accepts explicit loopback or private IP
addresses; there is no public listener or discovery. Internet rooms use HTTPS to
a relay chosen by the host. The invitation grants room access to anyone who has
it; end the room to invalidate it. Invitations are not saved to disk or included
in room snapshots.

The host serializes queue changes and assigns stable entry IDs. Retried add
operations with the same command ID are idempotent. Guests cannot publish host
playback, start the next shared track, or remove other guests' tracks through the
protocol. Rooms allow 16 members (8 listeners on a relay), 100 queued tracks, and
20 single songs or 50 album/playlist entries per guest; message sizes, handshake
times and pending connections are bounded.

## Verification and next milestones

Automated checks cover actual TCP host/guest exchanges, invalid invitations,
host shutdown, queue ownership and limits, duplicate adds, bounded messages, and
the follower's drift, stale-update and retry policies. Native screenshots cover
setup, hosting, following and minimum-window layouts using synthetic fixtures.

**Real Spotify playback on two devices has not been verified.** The next live
check is two separate Premium accounts on a private network: join, load the host
track, pause/resume, seek, change tracks, contribute a track, then end the room.
Observe audible offset, failed loads and account/session behavior before treating
the feature as ready for daily use.

Live checks pending: two-account audible sync and scheduled starts, Tide ↔ Grove
audio, a real public relay behind TLS, and host migration across real Wi-Fi.
Official Spotify Jam compatibility is outside this feature.
