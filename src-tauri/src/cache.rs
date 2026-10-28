use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::LsResponse;

/// How long a listing is reused while navigating. The refresh button, and
/// every change made through the app, fetch a new one.
const TTL: Duration = Duration::from_secs(300);

/// `o2cloud ls` downloads the whole media list on every call, whatever
/// folder is asked for (about 14 s on a 46,000-file account). Keep the last
/// listing of each folder so going back, and the virtual camera views, which
/// all read the root, don't pay for it again.
#[derive(Default)]
pub struct ListingCache {
    entries: Mutex<HashMap<String, (Instant, LsResponse)>>,
}

impl ListingCache {
    pub fn get(&self, key: &str) -> Option<LsResponse> {
        let entries = self.entries.lock().unwrap();
        let (fetched_at, listing) = entries.get(key)?;
        (fetched_at.elapsed() < TTL).then(|| listing.clone())
    }

    pub fn put(&self, key: String, listing: LsResponse) {
        self.entries
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), listing));
    }

    /// Update a cached listing in place. Returns whether it was found and
    /// still fresh; a change that only affects one folder can then keep the
    /// other cached listings instead of clearing them all.
    pub fn update(&self, key: &str, update: impl FnOnce(&mut LsResponse)) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some((fetched_at, listing)) = entries.get_mut(key) else {
            return false;
        };
        if fetched_at.elapsed() >= TTL {
            entries.remove(key);
            return false;
        }
        update(listing);
        true
    }

    /// Remove deleted entries from every cached listing, instead of clearing
    /// the whole cache. A deleted folder's own cached listing is dropped; the
    /// listings of folders that contained it are updated in place.
    pub fn remove_entries(&self, file_ids: &HashSet<u64>, folder_ids: &HashSet<u64>) {
        let mut entries = self.entries.lock().unwrap();
        for (_, (_, listing)) in entries.iter_mut() {
            listing
                .folders
                .retain(|folder| !folder_ids.contains(&folder.id));
            listing
                .files
                .retain(|file| !file_ids.contains(&file.id));
        }
        for id in folder_ids {
            entries.remove(&format!("id:{id}"));
        }
    }

    /// Forget every listing. Called after anything the app changes in O2:
    /// a move or delete can affect folders other than the one on screen.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RemoteFile, RemoteFolder};
    use std::collections::HashSet;

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
        let updated = cache.get("path:/").expect("cached listing");
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

        let root = cache.get("path:/").expect("root listing");
        assert!(root.folders.is_empty());
        assert!(root.files.is_empty());
        assert!(cache.get("id:1").is_none());
        assert!(cache.get("path:/Otra").is_some());
    }
}
