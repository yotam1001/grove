//! Every way a jam invitation can arrive, including `grove://` links.
//!
//! * `grove-jam://192.168.1.20:5000/<token>`: nearby room (token keys encryption).
//! * `tide-jam:v1.<base64>`: Tide relay room. `grove-jam:v2.<base64>`: cross-service relay room.
//! * `ABCD`: four-letter code on Tide's free shared relay.
//! * `grove://join?invite=<any of the above, percent-encoded>`: opened by the
//!   per-user URL handler the installer registers. Opening a link only fills in
//!   the invitation; the listener still chooses Join.
use crate::relay;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug)]
pub enum Invite {
    Nearby { address: SocketAddr, token: String },
    Relay { invite: relay::Invite, protocol: u8 },
    Code(String),
}

pub fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    }
}

pub fn nearby(address: SocketAddr, token: &str) -> String {
    format!("grove-jam://{address}/{token}")
}

/// A clickable form of any invite. Kept below Windows' command-line limits.
pub fn link(invite: &str) -> String {
    let mut url = url::Url::parse("grove://join").expect("static URL");
    url.query_pairs_mut().append_pair("invite", invite.trim());
    url.into()
}

/// Unwrap a `grove://join?invite=…` link; anything else is returned unchanged.
pub fn unwrap_link(input: &str) -> Result<String, String> {
    let input = input.trim();
    if !input.to_ascii_lowercase().starts_with("grove://") {
        return Ok(input.into());
    }
    if input.len() > 4096 {
        return Err("This jam link is too long.".into());
    }
    let url = url::Url::parse(input).map_err(|_| "This jam link is not valid.")?;
    if url.host_str() != Some("join")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !(url.path().is_empty() || url.path() == "/")
        || url.fragment().is_some()
    {
        return Err("This jam link is not valid.".into());
    }
    let mut pairs = url.query_pairs();
    let invite = match (pairs.next(), pairs.next()) {
        (Some((key, value)), None) if key == "invite" => value.into_owned(),
        _ => return Err("This jam link is not valid.".into()),
    };
    if invite.to_ascii_lowercase().starts_with("grove://") {
        return Err("This jam link is not valid.".into());
    }
    Ok(invite)
}

pub fn parse(input: &str) -> Result<Invite, String> {
    let input = unwrap_link(input)?;
    let input = input.trim();
    if input.starts_with("grove-jam://") {
        let (address, token) = parse_nearby(input)?;
        return Ok(Invite::Nearby { address, token });
    }
    if input.starts_with("tide-jam:") || input.starts_with("grove-jam:") {
        let (invite, protocol) = relay::Invite::from_code(input)?;
        return Ok(Invite::Relay { invite, protocol });
    }
    let code = input.to_ascii_uppercase();
    if relay::valid_short_code(&code) {
        return Ok(Invite::Code(code));
    }
    Err("Paste a Tide or Grove jam invitation.".into())
}

fn parse_nearby(input: &str) -> Result<(SocketAddr, String), String> {
    let url = url::Url::parse(input).map_err(|_| "Paste a Tide or Grove jam invitation.")?;
    if url.scheme() != "grove-jam"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid jam invitation.".into());
    }
    let ip: IpAddr = url
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .parse()
        .map_err(|_| "Jam invitations must use a local or private IP address.")?;
    let port = url
        .port()
        .filter(|p| *p > 0)
        .ok_or("Jam invitation is missing its port.")?;
    let token = url.path().strip_prefix('/').unwrap_or("");
    if !private(ip)
        || token.len() < 20
        || token.len() > 128
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Use a valid invitation from a nearby Tide or Grove host.".into());
    }
    Ok((SocketAddr::new(ip, port), token.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitations_reject_public_addresses_and_embedded_credentials() {
        assert!(parse("grove-jam://127.0.0.1:9876/abcdefghijklmnopqrstuv").is_ok());
        for url in [
            "grove-jam://8.8.8.8:9876/abcdefghijklmnopqrstuv",
            "grove-jam://user@127.0.0.1:9876/abcdefghijklmnopqrstuv",
            "https://127.0.0.1:9876/abcdefghijklmnopqrstuv",
            "grove-jam://127.0.0.1:9876/short",
        ] {
            assert!(parse(url).is_err(), "{url}");
        }
    }
    #[test]
    fn links_wrap_every_invite_kind_once() {
        let nearby = "grove-jam://192.168.1.20:5000/abcdefghijklmnopqrstuv";
        let wrapped = link(nearby);
        assert!(wrapped.starts_with("grove://join?invite="));
        assert!(matches!(parse(&wrapped).unwrap(), Invite::Nearby { .. }));
        assert!(matches!(parse("abcd").unwrap(), Invite::Code(code) if code == "ABCD"));
        assert!(matches!(parse(&link("WXYZ")).unwrap(), Invite::Code(_)));
        for bad in [
            "grove://join?invite=a&invite=b",
            "grove://evil?invite=ABCD",
            "grove://join/extra?invite=ABCD",
            "grove://user@join?invite=ABCD",
            &link(&link("ABCD")),
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let relay = crate::relay::Invite {
            endpoint: "https://jam.example.test".into(),
            room: "0123456789abcdef0123456789abcdef".into(),
            invite: crate::relay::Secret::random().unwrap(),
        };
        let code = relay.to_code(crate::version::RELAY_TIDE).unwrap();
        assert!(matches!(
            parse(&link(&code)).unwrap(),
            Invite::Relay { protocol: 1, .. }
        ));
    }
}
