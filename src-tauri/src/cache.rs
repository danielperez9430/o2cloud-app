use std::collections::HashMap;
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

    /// Forget every listing. Called after anything the app changes in O2:
    /// a move or delete can affect folders other than the one on screen.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}
