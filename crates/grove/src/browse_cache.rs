//! Browse pages cached between launches to cut startup requests.
//!
//! Home and Library tab responses are kept per Spotify account with a short TTL:
//! a fresh entry is shown without asking Spotify, a stale one is shown at once and
//! refreshed. Recent searches live here too. Nothing in this file is a credential;
//! sign-out deletes it. Review modes never read or write it.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Shown without a request while younger than this.
pub const FRESH_SECONDS: u64 = 20 * 60;
/// Shown while refreshing until this old; dropped after.
pub const KEEP_SECONDS: u64 = 3 * 24 * 60 * 60;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_RECENT: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    saved: u64,
    data: Value,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowseCache {
    account: String,
    entries: HashMap<String, Entry>,
    pub recent_searches: Vec<String>,
    #[serde(skip)]
    enabled: bool,
}

#[derive(Debug, PartialEq)]
pub enum Hit {
    Fresh(Value),
    Stale(Value),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn data_file(name: &str) -> Option<PathBuf> {
    crate::preferences::data_dir().map(|dir| dir.join(name))
}

fn path() -> Option<PathBuf> {
    data_file("browse-cache.json")
}

/// Writes through a temporary file so a crash never leaves half a cache.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!("{stem}-{}.tmp", crate::jam::id()));
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

impl BrowseCache {
    /// A memory-only cache: used by review modes and tests.
    pub fn memory() -> Self {
        Self::default()
    }

    /// Loads the cache for `account`; another account's data is discarded.
    pub fn open(account: &str) -> Self {
        let mut cache = path()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_BYTES))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default();
        cache.enabled = true;
        cache.bind(account);
        cache
    }

    pub fn bind(&mut self, account: &str) {
        if self.account != account {
            self.entries.clear();
            self.recent_searches.clear();
            self.account = account.to_owned();
        }
        let oldest = now().saturating_sub(KEEP_SECONDS);
        self.entries.retain(|_, e| e.saved >= oldest);
        self.recent_searches
            .retain(|q| !q.trim().is_empty() && q.len() <= 300);
        self.recent_searches.truncate(MAX_RECENT);
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn get(&self, key: &str) -> Option<Hit> {
        self.get_at(key, now())
    }

    fn get_at(&self, key: &str, at: u64) -> Option<Hit> {
        if self.account.is_empty() {
            return None;
        }
        let entry = self.entries.get(key)?;
        let age = at.saturating_sub(entry.saved);
        if age < FRESH_SECONDS {
            Some(Hit::Fresh(entry.data.clone()))
        } else if age < KEEP_SECONDS {
            Some(Hit::Stale(entry.data.clone()))
        } else {
            None
        }
    }

    pub fn put(&mut self, key: &str, data: &Value) {
        if self.account.is_empty() {
            return;
        }
        self.entries.insert(
            key.to_owned(),
            Entry {
                saved: now(),
                data: data.clone(),
            },
        );
        self.save();
    }

    /// Marks an entry stale so the next visit refreshes it (after a change).
    pub fn expire(&mut self, key: &str) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.saved = entry.saved.min(now().saturating_sub(FRESH_SECONDS));
        }
    }

    /// Most recent first, without duplicates (case-insensitive).
    pub fn remember_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() || query.len() > 300 {
            return;
        }
        self.recent_searches
            .retain(|q| !q.eq_ignore_ascii_case(query));
        self.recent_searches.insert(0, query.to_owned());
        self.recent_searches.truncate(MAX_RECENT);
        self.save();
    }

    pub fn forget_searches(&mut self) {
        self.recent_searches.clear();
        self.save();
    }

    /// Sign-out: forget everything, on disk too.
    pub fn wipe(&mut self) {
        self.entries.clear();
        self.recent_searches.clear();
        self.account.clear();
        if self.enabled {
            if let Some(path) = path() {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn save(&self) {
        if !self.enabled || self.account.is_empty() {
            return;
        }
        let Some(path) = path() else { return };
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        if bytes.len() as u64 <= MAX_BYTES {
            write_atomic(&path, &bytes);
        }
    }
}

/// A profile younger than this stands in for `/me` at startup.
pub const PROFILE_FRESH_SECONDS: u64 = 24 * 60 * 60;

/// What the startup needs from the last launch: the profile (`/me`) and the first
/// playlists page. The Spotify worker owns it, so it is a separate file from the
/// page cache the UI owns. It holds no token or credential; sign-out deletes it.
/// Review modes never start the worker's restore, so they never read it.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryCache {
    account: String,
    profile: Option<Entry>,
    playlists: Option<Entry>,
    #[serde(skip)]
    file: Option<PathBuf>,
}

impl LibraryCache {
    pub fn memory() -> Self {
        Self::default()
    }

    /// Loads the startup cache for `account`; another account's data is discarded.
    pub fn open(account: &str) -> Self {
        Self::open_at(data_file("library-cache.json"), account)
    }

    fn open_at(file: Option<PathBuf>, account: &str) -> Self {
        let mut cache = file
            .as_ref()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_BYTES))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default();
        cache.file = file;
        cache.bind(account);
        cache
    }

    fn bind(&mut self, account: &str) {
        if self.account != account {
            self.profile = None;
            self.playlists = None;
            self.account = account.to_owned();
        }
        let oldest = now().saturating_sub(KEEP_SECONDS);
        for slot in [&mut self.profile, &mut self.playlists] {
            if slot.as_ref().is_some_and(|e| e.saved < oldest) {
                *slot = None;
            }
        }
    }

    /// The cached `/me` reply, only when it belongs to this cache's account.
    pub fn profile(&self) -> Option<Hit> {
        self.profile_at(now())
    }

    fn profile_at(&self, at: u64) -> Option<Hit> {
        let entry = self.profile.as_ref()?;
        if self.account.is_empty() || entry.data["id"].as_str() != Some(self.account.as_str()) {
            return None;
        }
        hit(entry, at, PROFILE_FRESH_SECONDS)
    }

    pub fn playlists(&self) -> Option<Hit> {
        if self.account.is_empty() {
            return None;
        }
        hit(self.playlists.as_ref()?, now(), FRESH_SECONDS)
    }

    pub fn put_profile(&mut self, profile: &Value) {
        // Only what Grove shows or decides with; nothing else from `/me`.
        let kept = serde_json::json!({
            "id": profile["id"],
            "display_name": profile["display_name"],
            "product": profile["product"],
        });
        if kept["id"].as_str() != Some(self.account.as_str()) {
            return;
        }
        self.profile = Some(Entry {
            saved: now(),
            data: kept,
        });
        self.save();
    }

    pub fn put_playlists(&mut self, page: &Value) {
        if self.account.is_empty() || page["account"].as_str() != Some(self.account.as_str()) {
            return;
        }
        let mut page = page.clone();
        if let Some(fields) = page.as_object_mut() {
            for transient in ["cached", "stale", "cooldown", "error", "load_failed"] {
                fields.remove(transient);
            }
        }
        self.playlists = Some(Entry {
            saved: now(),
            data: page,
        });
        self.save();
    }

    /// Sign-out: forget the startup library on disk, whether or not it was opened.
    pub fn forget() {
        if let Some(path) = data_file("library-cache.json") {
            let _ = std::fs::remove_file(path);
        }
    }

    fn save(&self) {
        let Some(path) = &self.file else { return };
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        if bytes.len() as u64 <= MAX_BYTES {
            write_atomic(path, &bytes);
        }
    }
}

fn hit(entry: &Entry, at: u64, fresh: u64) -> Option<Hit> {
    let age = at.saturating_sub(entry.saved);
    if age < fresh {
        Some(Hit::Fresh(entry.data.clone()))
    } else if age < KEEP_SECONDS {
        Some(Hit::Stale(entry.data.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entries_expire_and_belong_to_one_account() {
        let mut cache = BrowseCache::memory();
        assert!(cache.get("home").is_none(), "no account, no cache");
        cache.bind("ana");
        cache.put("home", &json!({"kind":"home"}));
        let saved = cache.entries["home"].saved;
        assert_eq!(
            cache.get_at("home", saved + 5),
            Some(Hit::Fresh(json!({"kind":"home"})))
        );
        assert!(matches!(
            cache.get_at("home", saved + FRESH_SECONDS),
            Some(Hit::Stale(_))
        ));
        assert!(cache.get_at("home", saved + KEEP_SECONDS).is_none());
        cache.expire("home");
        assert!(matches!(cache.get("home"), Some(Hit::Stale(_))));
        cache.remember_search("Forest");
        cache.bind("bob");
        assert!(
            cache.get("home").is_none(),
            "another account's pages leaked"
        );
        assert!(cache.recent_searches.is_empty());
    }

    #[test]
    fn recent_searches_are_bounded_newest_first_and_unique() {
        let mut cache = BrowseCache::memory();
        cache.bind("ana");
        for q in ["a", "b", "A", "  ", "c"] {
            cache.remember_search(q);
        }
        assert_eq!(cache.recent_searches, ["c", "A", "b"]);
        for i in 0..20 {
            cache.remember_search(&format!("q{i}"));
        }
        assert_eq!(cache.recent_searches.len(), MAX_RECENT);
        assert_eq!(cache.recent_searches[0], "q19");
        cache.wipe();
        assert!(cache.recent_searches.is_empty() && cache.account().is_empty());
        let json = serde_json::to_string(&cache).unwrap();
        assert!(!json.contains("token"));
    }

    #[test]
    fn the_startup_library_survives_a_relaunch_for_its_account_only() {
        let dir = std::env::temp_dir().join(format!("grove-library-cache-{}", crate::jam::id()));
        let file = dir.join("library-cache.json");
        let mut cache = LibraryCache::open_at(Some(file.clone()), "ana");
        assert!(cache.profile().is_none() && cache.playlists().is_none());
        cache.put_profile(&json!({"id":"ana","display_name":"Ana","product":"premium","email":"a@example.test","access_token":"x"}));
        cache.put_profile(&json!({"id":"bob","display_name":"Bob"}));
        cache.put_playlists(&json!({"account":"ana","items":[{"name":"Mix"}],"cooldown":{"seconds":5},"cached":true}));
        cache.put_playlists(&json!({"account":"bob","items":[]}));

        let reopened = LibraryCache::open_at(Some(file.clone()), "ana");
        let Some(Hit::Fresh(profile)) = reopened.profile() else {
            panic!("the profile was not kept");
        };
        assert_eq!(
            profile,
            json!({"id":"ana","display_name":"Ana","product":"premium"})
        );
        let Some(Hit::Fresh(page)) = reopened.playlists() else {
            panic!("the playlists were not kept");
        };
        assert_eq!(page, json!({"account":"ana","items":[{"name":"Mix"}]}));
        let saved = reopened.profile.as_ref().unwrap().saved;
        assert!(matches!(
            reopened.profile_at(saved + PROFILE_FRESH_SECONDS),
            Some(Hit::Stale(_))
        ));
        assert!(reopened.profile_at(saved + KEEP_SECONDS).is_none());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("token") && !text.contains("email"));

        let other = LibraryCache::open_at(Some(file.clone()), "bob");
        assert!(other.profile().is_none() && other.playlists().is_none());
        assert_eq!(other.account, "bob");
        let _ = std::fs::remove_dir_all(dir);
    }
}
