//! Shared upload library.
//!
//! Each device keeps a merged local copy and uploads an immutable snapshot of
//! the files it uploaded to `/.o2cloud-gui/library/` in O2. Every device
//! merges all snapshots, so a SHA-256 can be recognised anywhere in the
//! account without re-uploading the content. The `-gui` suffix leaves the
//! plain `.o2cloud` name free for the official app.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::cli;
use crate::transfer;
use crate::types::{LsResponse, RemoteFile};

/// Hidden folder in the O2 account. The app hides it from its listings.
pub const LIBRARY_FOLDER: &str = "/.o2cloud-gui/library";
/// Snapshot files are `library-<device>-<timestamp>-<random>.json`.
const SNAPSHOT_PREFIX: &str = "library-";
const SNAPSHOT_VERSION: u32 = 1;
/// O2's listings lag behind uploads; don't distrust a copy this recently
/// recorded just because a fresh account listing does not show it yet.
const PRUNE_GRACE_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LibrarySource {
    pub name: String,
    #[serde(default)]
    pub captured_at: Option<i64>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LibraryCopy {
    pub folder_id: Option<u64>,
    pub folder_path: String,
    pub remote_id: Option<u64>,
    pub remote_name: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub sources: Vec<LibrarySource>,
    #[serde(default)]
    pub copies: Vec<LibraryCopy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySnapshot {
    pub version: u32,
    pub device_id: String,
    pub created_at: i64,
    #[serde(default)]
    pub entries: Vec<LibraryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotState {
    pub id: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct NewUpload {
    pub sha256: String,
    pub size: u64,
    pub name: String,
    pub captured_at: Option<i64>,
    pub source: String,
    pub folder_id: Option<u64>,
    pub folder_path: String,
    pub remote_id: Option<u64>,
    pub remote_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryCopyView {
    pub folder_id: Option<u64>,
    pub folder_path: String,
    pub remote_id: Option<u64>,
    pub remote_name: String,
    pub display_name: String,
}

impl LibraryCopyView {
    pub fn from_copy(copy: &LibraryCopy) -> Self {
        Self {
            folder_id: copy.folder_id,
            folder_path: copy.folder_path.clone(),
            remote_id: copy.remote_id,
            remote_name: copy.remote_name.clone(),
            display_name: display_name(&copy.remote_name),
        }
    }

    pub fn from_remote_file(file: &RemoteFile) -> Option<Self> {
        let path = file.path.as_deref()?;
        Some(Self {
            folder_id: None,
            folder_path: parent_path(path),
            remote_id: Some(file.id),
            remote_name: file.name.clone(),
            display_name: display_name(&file.name),
        })
    }
}

fn display_name(name: &str) -> String {
    name.strip_suffix(".o2manifest.json")
        .unwrap_or(name)
        .to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryStats {
    pub files: u64,
    pub bytes: u64,
    pub copies: u64,
    pub devices: u64,
    pub last_sync: Option<i64>,
    pub pending_snapshot: bool,
    pub device_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredLibrary {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    entries: Vec<LibraryEntry>,
    #[serde(default)]
    snapshots: HashMap<String, SnapshotState>,
    #[serde(default)]
    folder_id: Option<u64>,
    #[serde(default)]
    last_sync: Option<i64>,
    #[serde(default)]
    pending_snapshot: bool,
}

pub struct UploadLibrary {
    path: PathBuf,
    device_id: String,
    entries: HashMap<String, LibraryEntry>,
    /// `name\0size\0captured_at` -> SHA-256 values.
    fingerprints: HashMap<String, Vec<String>>,
    snapshots: HashMap<String, SnapshotState>,
    folder_id: Option<u64>,
    last_sync: Option<i64>,
    pending_snapshot: bool,
}

impl UploadLibrary {
    pub fn load(app: &AppHandle) -> Self {
        let path = app
            .path()
            .app_config_dir()
            .map(|dir| dir.join("library.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("o2cloud-library.json"));
        let stored: StoredLibrary = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let device_id = if stored.device_id.trim().is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            stored.device_id
        };
        let mut library = Self {
            path,
            device_id,
            entries: stored
                .entries
                .into_iter()
                .map(|entry| (entry.sha256.clone(), entry))
                .collect(),
            fingerprints: HashMap::new(),
            snapshots: stored.snapshots,
            folder_id: stored.folder_id,
            last_sync: stored.last_sync,
            pending_snapshot: stored.pending_snapshot,
        };
        library.rebuild_indexes();
        library
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn folder_id(&self) -> Option<u64> {
        self.folder_id
    }

    pub fn set_folder_id(&mut self, folder_id: u64) {
        self.folder_id = Some(folder_id);
    }

    pub fn last_sync(&self) -> Option<i64> {
        self.last_sync
    }

    pub fn mark_synced(&mut self) {
        self.last_sync = Some(now_ms());
    }

    pub fn pending_snapshot(&self) -> bool {
        self.pending_snapshot
    }

    pub fn mark_snapshot_pending(&mut self) {
        self.pending_snapshot = true;
    }

    pub fn mark_snapshot_done(&mut self) {
        self.pending_snapshot = false;
    }

    /// Drop every hash and snapshot reference. The device ID is kept so this
    /// device can publish a fresh snapshot later.
    pub fn forget(&mut self) {
        self.entries.clear();
        self.fingerprints.clear();
        self.snapshots.clear();
        self.folder_id = None;
        self.last_sync = None;
        self.pending_snapshot = false;
    }

    pub fn snapshots(&self) -> &HashMap<String, SnapshotState> {
        &self.snapshots
    }

    pub fn stats(&self) -> LibraryStats {
        let devices: HashSet<&str> = self
            .entries
            .values()
            .flat_map(|entry| entry.copies.iter().map(|copy| copy.device_id.as_str()))
            .filter(|device| !device.is_empty())
            .collect();
        LibraryStats {
            files: self.entries.len() as u64,
            bytes: self.entries.values().map(|entry| entry.size).sum(),
            copies: self
                .entries
                .values()
                .map(|entry| entry.copies.len() as u64)
                .sum(),
            devices: devices.len() as u64,
            last_sync: self.last_sync,
            pending_snapshot: self.pending_snapshot,
            device_id: self.device_id.clone(),
        }
    }

    pub fn lookup_hash(&self, sha256: &str) -> Option<&LibraryEntry> {
        self.entries.get(sha256)
    }

    /// Whether a copy of `remote_name` is already recorded in this folder.
    pub fn has_copy(&self, folder_path: &str, remote_name: &str) -> bool {
        self.entries.values().any(|entry| {
            entry
                .copies
                .iter()
                .any(|copy| copy.folder_path == folder_path && copy.remote_name == remote_name)
        })
    }

    pub fn lookup_fingerprint(
        &self,
        name: &str,
        size: u64,
        captured_at: Option<i64>,
    ) -> Vec<&LibraryEntry> {
        self.fingerprints
            .get(&fingerprint(name, size, captured_at))
            .map(|hashes| {
                hashes
                    .iter()
                    .filter_map(|hash| self.entries.get(hash))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Add the files a batch just uploaded and rebuild the lookup indexes.
    pub fn add_uploads(&mut self, uploads: Vec<NewUpload>) {
        for upload in uploads {
            let entry = self
                .entries
                .entry(upload.sha256.clone())
                .or_insert_with(|| LibraryEntry {
                    sha256: upload.sha256.clone(),
                    size: upload.size,
                    sources: Vec::new(),
                    copies: Vec::new(),
                });
            entry.size = upload.size;
            let source = LibrarySource {
                name: upload.name,
                captured_at: upload.captured_at,
                source: upload.source,
            };
            if !entry.sources.contains(&source) {
                entry.sources.push(source);
            }
            let copy = LibraryCopy {
                folder_id: upload.folder_id,
                folder_path: upload.folder_path,
                remote_id: upload.remote_id,
                remote_name: upload.remote_name,
                device_id: self.device_id.clone(),
                recorded_at: now_ms(),
            };
            let duplicate =
                entry
                    .copies
                    .iter()
                    .any(|existing| match (existing.remote_id, copy.remote_id) {
                        (Some(left), Some(right)) => left == right,
                        _ => {
                            existing.folder_path == copy.folder_path
                                && existing.remote_name == copy.remote_name
                        }
                    });
            if !duplicate {
                entry.copies.push(copy);
            }
        }
        self.pending_snapshot = true;
        self.rebuild_indexes();
    }

    /// Merge a snapshot downloaded from O2. Returns true when something was
    /// new, so the caller can decide whether to save the local state.
    pub fn merge_snapshot(&mut self, snapshot: LibrarySnapshot) -> bool {
        let mut changed = false;
        for entry in snapshot.entries {
            if entry.sha256.trim().is_empty() {
                continue;
            }
            let target = self
                .entries
                .entry(entry.sha256.clone())
                .or_insert_with(|| LibraryEntry {
                    sha256: entry.sha256.clone(),
                    size: entry.size,
                    sources: Vec::new(),
                    copies: Vec::new(),
                });
            if target.size != entry.size {
                // Same content hash with a different size means a corrupt
                // snapshot; keep the existing entry.
                continue;
            }
            for source in entry.sources {
                if !target.sources.contains(&source) {
                    target.sources.push(source);
                    changed = true;
                }
            }
            for copy in entry.copies {
                let duplicate = target.copies.iter().any(|existing| {
                    match (existing.remote_id, copy.remote_id) {
                        (Some(left), Some(right)) => left == right,
                        _ => {
                            existing.folder_path == copy.folder_path
                                && existing.remote_name == copy.remote_name
                        }
                    }
                });
                if !duplicate {
                    target.copies.push(copy);
                    changed = true;
                }
            }
        }
        if changed {
            self.rebuild_indexes();
        }
        changed
    }

    pub fn remember_snapshot(&mut self, name: String, state: SnapshotState) {
        self.snapshots.insert(name, state);
    }

    pub fn forget_snapshot(&mut self, name: &str) {
        self.snapshots.remove(name);
    }

    /// Drop copies that no longer exist in a fresh `ls -a` listing. Entries
    /// with no remaining copy are removed. Returns true when something
    /// changed.
    pub fn prune_missing(
        &mut self,
        remote_ids: &HashSet<u64>,
        remote_paths: &HashSet<String>,
    ) -> bool {
        let before_entries = self.entries.len();
        let before_copies: usize = self.entries.values().map(|entry| entry.copies.len()).sum();
        self.entries.retain(|_, entry| {
            entry.copies.retain(|copy| {
                if copy.recorded_at > 0 && now_ms() - copy.recorded_at < PRUNE_GRACE_MS {
                    return true;
                }
                if let Some(id) = copy.remote_id {
                    remote_ids.contains(&id)
                } else {
                    remote_paths.contains(&join_path(&copy.folder_path, &copy.remote_name))
                }
            });
            !entry.copies.is_empty()
        });
        let after_copies: usize = self.entries.values().map(|entry| entry.copies.len()).sum();
        let changed = before_entries != self.entries.len() || before_copies != after_copies;
        if changed {
            self.rebuild_indexes();
        }
        changed
    }

    /// The snapshot this device owns: entries with at least one copy made here.
    pub fn own_snapshot(&self) -> LibrarySnapshot {
        let entries = self
            .entries
            .values()
            .filter(|entry| {
                entry
                    .copies
                    .iter()
                    .any(|copy| copy.device_id == self.device_id)
            })
            .cloned()
            .collect();
        LibrarySnapshot {
            version: SNAPSHOT_VERSION,
            device_id: self.device_id.clone(),
            created_at: now_ms(),
            entries,
        }
    }

    /// Snapshot containing only the entries from one upload batch. Devices
    /// merge snapshots, so a small delta is enough until compaction.
    pub fn snapshot_from_uploads(&self, uploads: &[NewUpload]) -> LibrarySnapshot {
        let mut entries: HashMap<String, LibraryEntry> = HashMap::new();
        for upload in uploads {
            if upload.sha256.trim().is_empty() {
                continue;
            }
            let entry = entries
                .entry(upload.sha256.clone())
                .or_insert_with(|| LibraryEntry {
                    sha256: upload.sha256.clone(),
                    size: upload.size,
                    sources: Vec::new(),
                    copies: Vec::new(),
                });
            entry.size = upload.size;
            let source = LibrarySource {
                name: upload.name.clone(),
                captured_at: upload.captured_at,
                source: upload.source.clone(),
            };
            if !entry.sources.contains(&source) {
                entry.sources.push(source);
            }
            let copy = LibraryCopy {
                folder_id: upload.folder_id,
                folder_path: upload.folder_path.clone(),
                remote_id: upload.remote_id,
                remote_name: upload.remote_name.clone(),
                device_id: self.device_id.clone(),
                recorded_at: now_ms(),
            };
            let duplicate =
                entry
                    .copies
                    .iter()
                    .any(|existing| match (existing.remote_id, copy.remote_id) {
                        (Some(left), Some(right)) => left == right,
                        _ => {
                            existing.folder_path == copy.folder_path
                                && existing.remote_name == copy.remote_name
                        }
                    });
            if !duplicate {
                entry.copies.push(copy);
            }
        }
        LibrarySnapshot {
            version: SNAPSHOT_VERSION,
            device_id: self.device_id.clone(),
            created_at: now_ms(),
            entries: entries.into_values().collect(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let stored = StoredLibrary {
            version: SNAPSHOT_VERSION,
            device_id: self.device_id.clone(),
            entries: self.entries.values().cloned().collect(),
            snapshots: self.snapshots.clone(),
            folder_id: self.folder_id,
            last_sync: self.last_sync,
            pending_snapshot: self.pending_snapshot,
        };
        let raw = serde_json::to_vec_pretty(&stored).map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, raw).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&temporary, &self.path).map_err(|error| error.to_string())
    }

    fn rebuild_indexes(&mut self) {
        self.fingerprints.clear();
        for entry in self.entries.values() {
            for source in &entry.sources {
                let key = fingerprint(&source.name, entry.size, source.captured_at);
                let hashes = self.fingerprints.entry(key).or_default();
                if !hashes.contains(&entry.sha256) {
                    hashes.push(entry.sha256.clone());
                }
            }
        }
    }
}

pub fn fingerprint(name: &str, size: u64, captured_at: Option<i64>) -> String {
    match captured_at {
        Some(date) => format!("{name}\u{0}{size}\u{0}{date}"),
        None => format!("{name}\u{0}{size}"),
    }
}

pub fn is_internal_name(name: &str) -> bool {
    name == ".o2cloud-gui" || name.starts_with(".o2cloud-gui")
}

pub fn is_internal_path(path: &str) -> bool {
    path.trim_start_matches('/').starts_with(".o2cloud-gui")
}

pub fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

pub fn parent_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some(("", _)) => "/".into(),
        Some((parent, _)) if !parent.is_empty() => parent.into(),
        _ => "/".into(),
    }
}

/// Ensure `/.o2cloud-gui/library` exists and return its folder ID.
pub async fn ensure_folder(app: &AppHandle, cli_path: &Path) -> Result<u64, String> {
    let args = vec![
        "mkdir".to_string(),
        "-p".to_string(),
        LIBRARY_FOLDER.to_string(),
    ];
    let output = cli::run(app, cli_path, &args).await?;
    let text = output.combined();
    // `mkdir -p` may create `/.o2cloud-gui` and `/.o2cloud-gui/library`; the
    // target is the last ID in the output, not the first parent.
    if let Some(id) = parse_last_created_id(&text) {
        return Ok(id);
    }
    // Older CLI output variants: find the folder in its parent listing.
    let args = vec![
        "ls".to_string(),
        "--json".to_string(),
        "/.o2cloud-gui".to_string(),
    ];
    let listing: LsResponse = cli::run_json(app, cli_path, &args).await?;
    listing
        .folders
        .into_iter()
        .find(|folder| folder.name == "library")
        .map(|folder| folder.id)
        .ok_or_else(|| "No se ha podido crear la carpeta de la biblioteca en O2".to_string())
}

fn parse_last_created_id(raw: &str) -> Option<u64> {
    let marker = "(id:";
    let start = raw.rfind(marker)? + marker.len();
    let rest = &raw[start..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

pub async fn list_snapshots(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: u64,
) -> Result<Vec<RemoteFile>, String> {
    let args = vec![
        "ls".to_string(),
        "--json".to_string(),
        folder_id.to_string(),
    ];
    let listing: LsResponse = cli::run_json(app, cli_path, &args).await?;
    Ok(listing
        .files
        .into_iter()
        .filter(|file| file.name.starts_with(SNAPSHOT_PREFIX) && file.name.ends_with(".json"))
        .collect())
}

pub async fn download_snapshot(
    app: &AppHandle,
    cli_path: &Path,
    file: &RemoteFile,
) -> Result<LibrarySnapshot, String> {
    let directory = transfer::app_cache_dir(app)?.join("library");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let destination = directory.join(safe_component(&file.name));
    let _ = std::fs::remove_file(&destination);
    let args = vec![
        "download".to_string(),
        file.id.to_string(),
        "-o".to_string(),
        destination.to_string_lossy().to_string(),
    ];
    cli::run(app, cli_path, &args).await?;
    let raw = std::fs::read_to_string(&destination)
        .map_err(|error| format!("No se puede leer la biblioteca descargada: {error}"))?;
    let snapshot: LibrarySnapshot = serde_json::from_str(&raw)
        .map_err(|error| format!("Biblioteca con formato inválido: {error}"))?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(format!(
            "La biblioteca usa una versión no compatible ({}).",
            snapshot.version
        ));
    }
    Ok(snapshot)
}

/// Upload a snapshot to the library folder. Returns its remote name and ID
/// (when the CLI reports one).
pub async fn upload_snapshot(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: u64,
    snapshot: &LibrarySnapshot,
) -> Result<(String, Option<u64>, u64), String> {
    let directory = transfer::app_cache_dir(app)?.join("library");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let stamp = now_ms();
    let random = uuid::Uuid::new_v4().simple().to_string();
    let name = format!(
        "{SNAPSHOT_PREFIX}{}-{stamp}-{}.json",
        safe_component(snapshot.device_id.trim()),
        &random[..8]
    );
    let path = directory.join(&name);
    let raw = serde_json::to_vec_pretty(snapshot).map_err(|error| error.to_string())?;
    let size = raw.len() as u64;
    std::fs::write(&path, raw).map_err(|error| error.to_string())?;
    let args = vec![
        "upload".to_string(),
        path.to_string_lossy().to_string(),
        "--folder".to_string(),
        folder_id.to_string(),
    ];
    let output = cli::run(app, cli_path, &args).await?;
    let id = transfer::parse_created_id(&output.combined());
    Ok((name, id, size))
}

pub async fn delete_snapshot(app: &AppHandle, cli_path: &Path, id: u64) -> Result<(), String> {
    let args = vec!["rm".to_string(), id.to_string()];
    cli::run(app, cli_path, &args).await.map(|_| ())
}

/// Remove the snapshots downloaded for merging and the ones prepared here.
pub fn clear_cached_snapshots(app: &AppHandle) -> Result<(), String> {
    let directory = transfer::app_cache_dir(app)?.join("library");
    match std::fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// All snapshot files belonging to this device.
pub fn own_snapshot_files<'a>(files: &'a [RemoteFile], device_id: &str) -> Vec<&'a RemoteFile> {
    let prefix = format!("{SNAPSHOT_PREFIX}{}-", safe_component(device_id.trim()));
    files
        .iter()
        .filter(|file| file.name.starts_with(&prefix))
        .collect()
}

pub fn safe_component(value: &str) -> String {
    let component: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || character == '-'
                || character == '_'
                || character == '.'
            {
                character
            } else {
                '_'
            }
        })
        .collect();
    if component.is_empty() {
        "device".into()
    } else {
        component
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, name: &str, date: Option<i64>, folder: &str, id: u64) -> LibraryEntry {
        LibraryEntry {
            sha256: hash.into(),
            size: 10,
            sources: vec![LibrarySource {
                name: name.into(),
                captured_at: date,
                source: "photos".into(),
            }],
            copies: vec![LibraryCopy {
                folder_id: None,
                folder_path: folder.into(),
                remote_id: Some(id),
                remote_name: name.into(),
                device_id: "device-a".into(),
                recorded_at: 1,
            }],
        }
    }

    #[test]
    fn fingerprints_match_photos_by_name_size_and_date() {
        assert_eq!(
            fingerprint("IMG_1.MOV", 10, Some(123)),
            "IMG_1.MOV\u{0}10\u{0}123"
        );
        assert_eq!(fingerprint("IMG_1.MOV", 10, None), "IMG_1.MOV\u{0}10");
    }

    #[test]
    fn merging_snapshots_keeps_all_copies_and_sources() {
        let mut library = UploadLibrary {
            path: PathBuf::from("/tmp/o2cloud-library-test.json"),
            device_id: "device-b".into(),
            entries: HashMap::new(),
            fingerprints: HashMap::new(),
            snapshots: HashMap::new(),
            folder_id: None,
            last_sync: None,
            pending_snapshot: false,
        };
        let first = entry("abc", "IMG_1.MOV", Some(123), "/A", 1);
        let mut second = entry("abc", "IMG_1.MOV", Some(123), "/B", 2);
        second.sources[0].name = "COPY.MOV".into();
        library.merge_snapshot(LibrarySnapshot {
            version: SNAPSHOT_VERSION,
            device_id: "device-a".into(),
            created_at: 1,
            entries: vec![first, second],
        });
        let merged = library.entries.get("abc").unwrap();
        assert_eq!(merged.copies.len(), 2);
        assert_eq!(merged.sources.len(), 2);
        assert_eq!(
            library.lookup_fingerprint("IMG_1.MOV", 10, Some(123)).len(),
            1
        );
        assert_eq!(
            library.lookup_fingerprint("COPY.MOV", 10, Some(123)).len(),
            1
        );
    }

    #[test]
    fn forgetting_clears_hashes_snapshots_and_sync_state() {
        let mut library = UploadLibrary {
            path: PathBuf::from("/tmp/o2cloud-library-test.json"),
            device_id: "device-b".into(),
            entries: HashMap::new(),
            fingerprints: HashMap::new(),
            snapshots: HashMap::new(),
            folder_id: None,
            last_sync: None,
            pending_snapshot: false,
        };
        library.merge_snapshot(LibrarySnapshot {
            version: SNAPSHOT_VERSION,
            device_id: "device-a".into(),
            created_at: 1,
            entries: vec![entry("abc", "IMG_1.MOV", Some(123), "/A", 1)],
        });
        library.remember_snapshot(
            "library-device-a-1.json".into(),
            SnapshotState { id: 1, size: 10 },
        );
        library.set_folder_id(9);
        library.mark_synced();
        library.mark_snapshot_pending();

        library.forget();

        assert!(library.entries.is_empty());
        assert!(library.fingerprints.is_empty());
        assert!(library.snapshots.is_empty());
        assert!(library.folder_id.is_none());
        assert!(library.last_sync.is_none());
        assert!(!library.pending_snapshot());
        assert_eq!(library.device_id, "device-b");
        assert_eq!(library.stats().files, 0);
    }

    #[test]
    fn upload_batch_snapshot_contains_only_that_batch() {
        let library = UploadLibrary {
            path: PathBuf::from("/tmp/o2cloud-library-test.json"),
            device_id: "device-b".into(),
            entries: HashMap::new(),
            fingerprints: HashMap::new(),
            snapshots: HashMap::new(),
            folder_id: None,
            last_sync: None,
            pending_snapshot: false,
        };

        let snapshot = library.snapshot_from_uploads(&[NewUpload {
            sha256: "abc".into(),
            size: 10,
            name: "IMG_1.MOV".into(),
            captured_at: Some(123),
            source: "photos".into(),
            folder_id: Some(7),
            folder_path: "/A".into(),
            remote_id: Some(1),
            remote_name: "IMG_1.MOV".into(),
        }]);

        assert_eq!(snapshot.device_id, "device-b");
        assert_eq!(snapshot.entries.len(), 1);
        let entry = &snapshot.entries[0];
        assert_eq!(entry.sha256, "abc");
        assert_eq!(entry.copies.len(), 1);
        assert_eq!(entry.copies[0].device_id, "device-b");
        assert_eq!(entry.copies[0].folder_path, "/A");
        assert_eq!(entry.sources[0].source, "photos");
    }

    #[test]
    fn pruning_removes_copies_that_are_no_longer_in_o2() {
        let mut library = UploadLibrary {
            path: PathBuf::from("/tmp/o2cloud-library-test.json"),
            device_id: "device-b".into(),
            entries: HashMap::new(),
            fingerprints: HashMap::new(),
            snapshots: HashMap::new(),
            folder_id: None,
            last_sync: None,
            pending_snapshot: false,
        };
        library.merge_snapshot(LibrarySnapshot {
            version: SNAPSHOT_VERSION,
            device_id: "device-a".into(),
            created_at: 1,
            entries: vec![entry("abc", "IMG_1.MOV", Some(123), "/A", 1)],
        });
        let ids = HashSet::from([2]);
        let paths = HashSet::new();
        assert!(library.prune_missing(&ids, &paths));
        assert!(library.entries.is_empty());
    }

    #[test]
    fn local_state_is_written_atomically_and_contains_the_new_entry() {
        let path = std::env::temp_dir().join(format!(
            "o2cloud-library-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut library = UploadLibrary {
            path: path.clone(),
            device_id: "device-test".into(),
            entries: HashMap::new(),
            fingerprints: HashMap::new(),
            snapshots: HashMap::new(),
            folder_id: None,
            last_sync: None,
            pending_snapshot: false,
        };
        library.add_uploads(vec![NewUpload {
            sha256: "abc".into(),
            size: 10,
            name: "IMG_1.MOV".into(),
            captured_at: Some(123),
            source: "photos".into(),
            folder_id: Some(1),
            folder_path: "/A".into(),
            remote_id: Some(2),
            remote_name: "IMG_1.MOV".into(),
        }]);
        library.save().unwrap();
        let stored: StoredLibrary =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(stored.entries[0].sha256, "abc");
        assert!(stored.pending_snapshot);
        assert_eq!(stored.device_id, "device-test");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mkdir_with_parents_uses_the_target_folder_id() {
        let output = "✓ Created folder /.o2cloud-gui/ (id:35854786)\n✓ Created folder /.o2cloud-gui/library/ (id:35854787)\n";
        assert_eq!(parse_last_created_id(output), Some(35854787));
        assert_eq!(
            parse_last_created_id("✓ /.o2cloud-gui/library/ already exists (id: 35854787)"),
            Some(35854787)
        );
    }

    #[test]
    fn internal_paths_are_hidden_from_listings() {
        assert!(is_internal_name(".o2cloud-gui"));
        assert!(is_internal_name(".o2cloud-gui-extra"));
        assert!(!is_internal_name(".o2cloud"));
        assert!(!is_internal_name("Vídeos"));
        assert!(is_internal_path("/.o2cloud-gui/library/x.json"));
        assert!(is_internal_path(".o2cloud-gui/library/x.json"));
        assert!(!is_internal_path("/.o2cloud/library/x.json"));
        assert!(!is_internal_path("/Vídeos/x.json"));
    }
}
