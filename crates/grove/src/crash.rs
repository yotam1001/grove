//! Local crash reports, ported from Tide's crash_log.rs.
//!
//! A panic hook writes the last panic to `%APPDATA%\GroveMusic\crash.log`.
//! Release builds have no console, so without it a crash leaves no trace. The
//! report holds the version, thread, panic message, location and backtrace,
//! after redaction: the user's home folder and name, URLs past their host,
//! Spotify IDs, e-mail addresses, token-like strings, and the account name and
//! song titles Grove showed recently are replaced. Nothing is uploaded; the
//! next launch offers the report with Copy and Dismiss.
use crate::{GroveSystem, GroveWindow};
use slint::ComponentHandle;
use std::{
    fs,
    io::{Read, Write},
    panic::PanicHookInfo,
    path::PathBuf,
    sync::Mutex,
};

const LIMIT: usize = 64 * 1024;
const MAX_SENSITIVE: usize = 64;

/// Values shown in the UI that must never reach a report (account name,
/// recent titles and artists). Bounded; the oldest are forgotten first.
static SENSITIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn remember(value: &str) {
    let value = value.trim();
    if value.chars().count() < 3 || value.len() > 300 {
        return;
    }
    let mut list = SENSITIVE.lock().unwrap_or_else(|p| p.into_inner());
    if !list.iter().any(|known| known == value) {
        if list.len() == MAX_SENSITIVE {
            list.remove(0);
        }
        list.push(value.to_owned());
    }
}

pub fn path() -> Option<PathBuf> {
    crate::preferences::data_dir().map(|dir| dir.join("crash.log"))
}

/// Chain a hook that records each panic after the default output. Review
/// modes never install it, so they cannot leave files behind.
pub fn install(offline: bool) {
    if offline {
        return;
    }
    let Some(path) = path() else {
        return;
    };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        // Never block inside a panic: skip the extra names if the list is busy.
        let sensitive = SENSITIVE.try_lock().map(|l| l.clone()).unwrap_or_default();
        let text = redact(
            &report(info, &thread, &backtrace),
            &sensitive,
            &Identity::current(),
        );
        if let Some(folder) = path.parent() {
            let _ = fs::create_dir_all(folder);
        }
        if let Ok(mut file) = fs::File::create(&path) {
            let _ = file.write_all(bounded(text).as_bytes());
        }
    }));
}

fn report(info: &PanicHookInfo<'_>, thread: &str, backtrace: &str) -> String {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic".into());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".into());
    format!(
        "Grove {} crashed on {} {}\nthread: {thread}\nmessage: {message}\nlocation: {location}\n\n{backtrace}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn bounded(mut text: String) -> String {
    if text.len() > LIMIT {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated]");
    }
    text
}

/// The local identity to scrub from reports.
#[derive(Default)]
pub struct Identity {
    home: Option<String>,
    user: Option<String>,
}

impl Identity {
    fn current() -> Self {
        Self {
            home: std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .ok(),
            user: std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .ok(),
        }
    }
}

fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() || !needle.is_ascii() {
        return text.replace(needle, with);
    }
    let lower = text.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, _) in lower.match_indices(&needle) {
        if start < last {
            continue;
        }
        out.push_str(&text[last..start]);
        out.push_str(with);
        last = start + needle.len();
    }
    out.push_str(&text[last..]);
    out
}

fn secret_like(run: &str) -> bool {
    run.len() >= 24
        && run.bytes().any(|b| b.is_ascii_digit())
        && run.bytes().any(|b| b.is_ascii_alphabetic())
}

fn redact_word(word: &str, after_bearer: bool) -> String {
    if after_bearer {
        return "<redacted>".into();
    }
    if let Some(scheme_end) = word.find("://") {
        let rest = &word[scheme_end + 3..];
        let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let host = &rest[..host_end];
        let host = host.rsplit('@').next().unwrap_or(host);
        let tail = if host_end < rest.len() {
            "/<redacted>"
        } else {
            ""
        };
        return format!("{}://{host}{tail}", &word[..scheme_end]);
    }
    if let Some(at) = word.find('@') {
        if word[at + 1..].contains('.') && at > 0 {
            return "<email>".into();
        }
    }
    let mut word = word.to_owned();
    if let Some(start) = word.to_ascii_lowercase().find("spotify:") {
        word.replace_range(start.., "spotify:<redacted>");
    }
    // Replace token-like runs; `.`, `/` and `:` split runs so file paths and
    // Rust symbol paths keep their shape.
    let mut out = String::with_capacity(word.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if secret_like(run) {
            out.push_str("<redacted>");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in word.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=') {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Remove account, history and credential-like content from a report.
pub fn redact(text: &str, sensitive: &[String], identity: &Identity) -> String {
    // Word pass first, so links and addresses are judged before names inside
    // them are replaced.
    let separators = |c: char| c.is_whitespace() || "\"'`,;()[]{}<>|".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut previous_word = String::new();
    for c in text.chars() {
        if separators(c) {
            if !word.is_empty() {
                let after_bearer = previous_word.eq_ignore_ascii_case("bearer");
                out.push_str(&redact_word(&word, after_bearer));
                previous_word = std::mem::take(&mut word);
            }
            out.push(c);
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push_str(&redact_word(
            &word,
            previous_word.eq_ignore_ascii_case("bearer"),
        ));
    }
    // Longest first, so a title containing a shorter remembered value goes whole.
    let mut sensitive: Vec<&String> = sensitive
        .iter()
        .filter(|s| s.chars().count() >= 3)
        .collect();
    sensitive.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for value in sensitive {
        out = out.replace(value.as_str(), "<redacted>");
    }
    if let Some(home) = identity.home.as_deref().filter(|h| h.len() >= 3) {
        out = replace_ignore_case(&out, home, "<home>");
        out = replace_ignore_case(&out, &home.replace('\\', "/"), "<home>");
    }
    if let Some(user) = identity.user.as_deref().filter(|u| u.chars().count() >= 3) {
        out = replace_ignore_case(&out, user, "<user>");
    }
    out
}

/// Read a pending report, if any. Bounded and redacted again on load.
fn pending() -> Option<String> {
    let path = path()?;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 64).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(bounded(redact(&text, &[], &Identity::current())))
}

/// Offer last run's report once at launch. Offline reviews never read it.
pub fn offer(ui: &GroveWindow, offline: bool) {
    let system = ui.global::<GroveSystem>();
    if !offline {
        if let Some(report) = pending() {
            system.set_crash_report(report.into());
            system.set_crash_pending(true);
        }
    }
    let weak = ui.as_weak();
    system.on_crash_action(move |action| {
        let Some(ui) = weak.upgrade() else { return };
        let system = ui.global::<GroveSystem>();
        match action.as_str() {
            // The Slint side copies the text; this only confirms it.
            "copy" => {
                system.set_crash_status("Copied. Paste it into a GitHub issue or an e-mail.".into())
            }
            "dismiss" => {
                if !offline {
                    if let Some(path) = path() {
                        let _ = fs::remove_file(path);
                    }
                }
                system.set_crash_pending(false);
                system.set_crash_report("".into());
                system.set_crash_status("".into());
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            home: Some(r"D:\Profiles\Example".into()),
            user: Some("Example".into()),
        }
    }

    #[test]
    fn report_is_bounded_on_a_character_boundary() {
        let text = bounded("é".repeat(LIMIT));
        assert!(text.len() <= LIMIT + "\n[truncated]".len());
        assert!(text.ends_with("[truncated]"));
        assert_eq!(bounded("short".into()), "short");
    }

    #[test]
    fn redaction_removes_identity_tokens_links_and_history() {
        let text = concat!(
            "Grove crashed\nmessage: called `Result::unwrap()` on an `Err` value: ",
            "\"https://api.spotify.com/v1/me/player?device_id=abc\" Bearer BQDf3x9 ",
            "token=AQB7d1e2f3g4h5i6j7k8l9m0n1o2p3q4 for listener@example.com ",
            "playing spotify:track:4uLU6hMCjMI75M1A2tKUQC by Evening Light from ",
            r"D:\Profiles\Example\AppData\Roaming\GroveMusic and D:/profiles/example/x ",
            "user example\nlocation: /workspace/crates/grove/src/main.rs:10:5\n",
            "   3: grove_music::features::Features::response::h1a2b3c4d5e6f7a8b\n",
        );
        let out = redact(
            text,
            &["Evening Light".into(), "Example Listener".into()],
            &identity(),
        );
        for leaked in [
            "/v1/me",
            "device_id",
            "BQDf3x9",
            "AQB7d1e2",
            "listener@example.com",
            "4uLU6hMCjMI75M1A2tKUQC",
            "Evening Light",
            "Example",
            "example",
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked:\n{out}");
        }
        for kept in [
            "https://api.spotify.com/<redacted>",
            "spotify:<redacted>",
            "<email>",
            "<home>",
            "location: /workspace/crates/grove/src/main.rs:10:5",
            "grove_music::features::Features::response::h1a2b3c4d5e6f7a8b",
            "called `Result::unwrap()`",
        ] {
            assert!(out.contains(kept), "{kept} missing:\n{out}");
        }
    }

    #[test]
    fn panic_message_and_location_are_recorded() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let text = report(info, "main", "frames");
            if text.contains("Synthetic crash fixture") {
                *sink.lock().unwrap() = text;
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("Synthetic crash fixture"));
        std::panic::set_hook(previous);
        let text = captured.lock().unwrap().clone();
        assert!(text.contains("message: Synthetic crash fixture"), "{text}");
        assert!(text.contains("crash.rs"), "{text}");
        assert!(
            text.contains("thread: main") && text.ends_with("frames"),
            "{text}"
        );
    }

    #[test]
    fn remembered_values_are_bounded_and_deduplicated() {
        for i in 0..(MAX_SENSITIVE + 5) {
            remember(&format!("Remembered value {i}"));
        }
        remember("Remembered value 70");
        remember("ab");
        let list = SENSITIVE.lock().unwrap();
        assert!(list.len() <= MAX_SENSITIVE);
        assert!(!list.iter().any(|v| v == "ab"));
        assert_eq!(
            list.iter().filter(|v| *v == "Remembered value 68").count(),
            1
        );
    }
}
