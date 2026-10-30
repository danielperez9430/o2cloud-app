use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::types::LsResponse;

/// How long a listing is reused without asking the CLI again.
const TTL: Duration = Duration::from_secs(300);
/// How long the last listings survive on disk, so the app opens with data.
const MAX_DISK_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Keep the disk cache small: the root listing alone can be several MB.
const MAX_DISK_ENTRIES: usize = 8;
const MAX_DISK_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CachedListing {
    pub listing: LsResponse,
    /// True when this came from disk and should be refreshed in the background.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntry {
    fetched_at_ms: u64,
    listing: LsResponse,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, StoredEntry>,
}

pub struct ListingCache {
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
    save_lock: Mutex<()>,
}

impl Default for ListingCache {
    fn default() -> Self {
        Self::load(None)
    }
}

impl ListingCache {
    pub fn load(path: Option<PathBuf>) -> Self {
        let stored = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<HashMap<String, StoredEntry>>(&raw).ok());
        let cache = Self {
            inner: Mutex::new(Inner::default()),
            path,
            save_lock: Mutex::new(()),
        };
        let Some(stored) = stored else {
            return cache;
        };
        let now = now_ms();
        {
            let mut inner = cache.inner.lock().unwrap();
            for (key, entry) in stored {
                if now.saturating_sub(entry.fetched_at_ms) < MAX_DISK_AGE.as_millis() as u64 {
                    inner.entries.insert(key, entry);
                }
            }
        }
        cache
    }

    pub fn get(&self, key: &str) -> Option<CachedListing> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.entries.get(key)?;
        let age = now_ms().saturating_sub(entry.fetched_at_ms);
        if age >= MAX_DISK_AGE.as_millis() as u64 {
            inner.entries.remove(key);
            drop(inner);
            self.save();
            return None;
        }
        let listing = entry.listing.clone();
        let stale = age >= TTL.as_millis() as u64;
        Some(CachedListing { listing, stale })
    }

    pub fn put(&self, key: String, listing: LsResponse) {
        self.inner.lock().unwrap().entries.insert(
            key,
            StoredEntry {
                fetched_at_ms: now_ms(),
                listing,
            },
        );
        self.save();
    }

    /// Update a cached listing in place. Returns whether it was found.
    pub fn update(&self, key: &str, update: impl FnOnce(&mut LsResponse)) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.entries.get_mut(key) else {
            return false;
        };
        let age = now_ms().saturating_sub(entry.fetched_at_ms);
        if age >= MAX_DISK_AGE.as_millis() as u64 {
            inner.entries.remove(key);
            drop(inner);
            self.save();
            return false;
        }
        update(&mut entry.listing);
        drop(inner);
        self.save();
        true
    }

    /// Remove deleted entries from every cached listing, instead of clearing
    /// the whole cache. A deleted folder's own cached listing is dropped; the
    /// listings of folders that contained it are updated in place.
    pub fn remove_entries(&self, file_ids: &HashSet<u64>, folder_ids: &HashSet<u64>) {
        let mut inner = self.inner.lock().unwrap();
        for entry in inner.entries.values_mut() {
            entry
                .listing
                .folders
                .retain(|folder| !folder_ids.contains(&folder.id));
            entry
                .listing
                .files
                .retain(|file| !file_ids.contains(&file.id));
        }
        for id in folder_ids {
            inner.entries.remove(&format!("id:{id}"));
        }
        drop(inner);
        self.save();
    }

    /// Forget every listing. Called after anything that can affect the
    /// account, such as login or logout.
    pub fn clear(&self) {
        self.inner.lock().unwrap().entries.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let _guard = self.save_lock.lock().unwrap();
        let now = now_ms();
        let mut entries: Vec<(String, StoredEntry)> = self
            .inner
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter(|(_, entry)| {
                now.saturating_sub(entry.fetched_at_ms) < MAX_DISK_AGE.as_millis() as u64
            })
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect();
        entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.fetched_at_ms));

        let mut kept = HashMap::new();
        let mut bytes = 0usize;
        for (key, entry) in entries {
            if kept.len() >= MAX_DISK_ENTRIES {
                break;
            }
            let Ok(encoded) = serde_json::to_vec(&entry) else {
                continue;
            };
            if !kept.is_empty() && bytes + encoded.len() > MAX_DISK_BYTES {
                break;
            }
            bytes += encoded.len();
            kept.insert(key, entry);
        }

        let Ok(raw) = serde_json::to_vec(&kept) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, raw).is_ok() {
            // On Windows a rename does not replace an existing file.
            #[cfg(windows)]
            {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RemoteFile, RemoteFolder};

    fn listing(path: &str) -> LsResponse {
        LsResponse {
            path: path.into(),
            folders: Vec::new(),
            files: Vec::new(),
        }
    }

    #[test]
    fn updates_a_cached_listing_in_place() {
        let cache = ListingCache::default();
        cache.put("path:/".into(), listing("/"));

        let found = cache.update("path:/", |listing| {
            listing.folders.push(RemoteFolder {
                id: 7,
                name: "Pruebas".into(),
                files: 0,
                size: 0,
            });
        });

        assert!(found);
        let updated = cache.get("path:/").expect("cached listing").listing;
        assert_eq!(updated.folders.len(), 1);
        assert_eq!(updated.folders[0].name, "Pruebas");
    }

    #[test]
    fn update_reports_a_missing_listing() {
        let cache = ListingCache::default();
        assert!(!cache.update("path:/", |_| {}));
    }

    #[test]
    fn removes_deleted_entries_without_clearing_other_folders() {
        let cache = ListingCache::default();
        let mut root = listing("/");
        root.folders.push(RemoteFolder {
            id: 1,
            name: "Prueba".into(),
            files: 0,
            size: 0,
        });
        root.files.push(RemoteFile {
            id: 10,
            name: "nota.txt".into(),
            size: Some(4),
            date: None,
            path: None,
        });
        cache.put("path:/".into(), root);
        cache.put("id:1".into(), listing("/Prueba"));
        cache.put("path:/Otra".into(), listing("/Otra"));

        cache.remove_entries(&HashSet::from([10]), &HashSet::from([1]));

        let root = cache.get("path:/").expect("root listing").listing;
        assert!(root.folders.is_empty());
        assert!(root.files.is_empty());
        assert!(cache.get("id:1").is_none());
        assert!(cache.get("path:/Otra").is_some());
    }

    #[test]
    fn persists_and_reloads_a_listing() {
        let path = std::env::temp_dir().join(format!(
            "o2cloud-listings-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let cache = ListingCache::load(Some(path.clone()));
        cache.put("path:/".into(), listing("/"));

        // The second save must also reach disk; Windows cannot rename over
        // an existing file.
        let mut updated = listing("/");
        updated.folders.push(RemoteFolder {
            id: 9,
            name: "Guardada".into(),
            files: 0,
            size: 0,
        });
        cache.put("path:/".into(), updated);

        let loaded = ListingCache::load(Some(path.clone()));
        let cached = loaded.get("path:/").expect("persisted listing");
        assert_eq!(cached.listing.path, "/");
        assert_eq!(cached.listing.folders[0].name, "Guardada");
        assert!(!cached.stale);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_disk_entries_are_marked_for_refreshing() {
        let cache = ListingCache::default();
        cache.inner.lock().unwrap().entries.insert(
            "path:/".into(),
            StoredEntry {
                fetched_at_ms: now_ms().saturating_sub(TTL.as_millis() as u64 + 1),
                listing: listing("/"),
            },
        );

        let cached = cache.get("path:/").expect("stale listing");
        assert!(cached.stale);
    }
}
