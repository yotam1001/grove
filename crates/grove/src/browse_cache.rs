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

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|root| root.join("GroveMusic").join("browse-cache.json"))
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
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        if bytes.len() as u64 > MAX_BYTES {
            return;
        }
        let temporary = parent.join(format!("browse-cache-{}.tmp", crate::jam::id()));
        if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, &path).is_err()
        {
            let _ = std::fs::remove_file(&temporary);
        }
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
}
