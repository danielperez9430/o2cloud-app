use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use walkdir::WalkDir;

use crate::cache::{CachedListing, ListingCache};
use crate::camera;
use crate::cli::{self, CliManager};
use crate::library::{self, NewUpload, UploadLibrary};
use crate::photos;
use crate::transfer::{self, ExistingFiles, UploadOutcome};
use crate::types::{
    AppSettings, ChunkManifest, CliInfo, CliRelease, LogicalFile, LsResponse, LogicalListing,
    PartRef, RemoteFile, RemoteFolder, SessionStatus, TransferProgress, VirtualFolder,
};

/// Each `rm` lists the whole account before deleting; a few at a time keeps
/// a chunked file's delete short without flooding O2.
const PARALLEL_DELETES: usize = 3;

/// The frontend opens the login window when it sees this.
const SESSION_EXPIRED: &str = "La sesión de O2 ha caducado. Vuelve a iniciar sesión.";

/// The whole-account listing (`ls -a --json`), reused for a few minutes.
#[derive(Default)]
struct AllFilesCache {
    at: Option<Instant>,
    files: Vec<RemoteFile>,
}

pub struct AppState {
    pub cli: CliManager,
    pub settings: RwLock<AppSettings>,
    pub library: RwLock<UploadLibrary>,
    listings: ListingCache,
    /// Uploads run one at a time, in the order they were started.
    upload_lock: Mutex<()>,
    /// One library sync at a time.
    library_sync_lock: Mutex<()>,
    all_files: RwLock<AllFilesCache>,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Self {
        let cli = CliManager::discover(app);
        let settings = load_settings(app);
        let listings = transfer::app_cache_dir(app)
            .ok()
            .map(|dir| dir.join("listings.json"));
        Self {
            cli,
            settings: RwLock::new(settings),
            library: RwLock::new(UploadLibrary::load(app)),
            listings: ListingCache::load(listings),
            upload_lock: Mutex::new(()),
            library_sync_lock: Mutex::new(()),
            all_files: RwLock::new(AllFilesCache::default()),
        }
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

fn load_settings(app: &AppHandle) -> AppSettings {
    let Ok(path) = settings_path(app) else {
        return AppSettings::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn normalize_settings(mut settings: AppSettings) -> AppSettings {
    settings.chunk_size_mb = settings.chunk_size_mb.clamp(256, 3800);
    settings
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".into()
    } else if trimmed.starts_with('/') {
        trimmed.trim_end_matches('/').to_string()
    } else {
        format!("/{}", trimmed.trim_end_matches('/'))
    }
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

#[tauri::command]
pub fn get_cli_info(state: State<'_, AppState>) -> Result<CliInfo, String> {
    Ok(state.cli.info())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings.read().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let settings = normalize_settings(settings);
    let raw = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(&app)?, raw).map_err(|e| e.to_string())?;
    *state.settings.write().unwrap() = settings.clone();
    Ok(settings)
}

#[tauri::command]
pub async fn get_session_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SessionStatus, String> {
    let path = state.cli.path_or_err()?;
    let args = vec!["status".to_string()];
    match cli::run(&app, &path, &args).await {
        Ok(output) => Ok(parse_status(&output.combined(), true)),
        Err(err) => {
            if err.contains("Not logged in") || err.contains("not logged") {
                Ok(parse_status(&err, false))
            } else {
                Err(err)
            }
        }
    }
}

fn parse_status(raw: &str, logged_in: bool) -> SessionStatus {
    let phone = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("Phone:"))
        .map(|s| s.trim().to_string());
    let storage = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("Storage:"))
        .map(|s| s.trim().to_string());
    SessionStatus {
        logged_in: logged_in && raw.contains("Logged in") && raw.contains("Session: active"),
        raw: raw.to_string(),
        phone,
        storage,
    }
}

/// Whether the CLI at `cli_path` has a session O2 accepts. `status` checks
/// without renewing it; any other command with an expired session opens
/// the CLI's login window.
async fn session_active(app: &AppHandle, cli_path: &Path) -> Result<bool, String> {
    let output = cli::run(app, cli_path, &["status".to_string()]).await?;
    Ok(parse_status(&output.combined(), true).logged_in)
}

/// Refuse a mutating CLI command when the session is expired: the CLI would
/// otherwise open its login window and the command would wait there.
async fn ensure_session(app: &AppHandle, cli_path: &Path) -> Result<(), String> {
    if session_active(app, cli_path).await? {
        Ok(())
    } else {
        Err(SESSION_EXPIRED.into())
    }
}

#[tauri::command]
pub async fn login(app: AppHandle, state: State<'_, AppState>) -> Result<SessionStatus, String> {
    let path = state.cli.path_or_err()?;
    let args = vec!["login".to_string()];
    let _ = cli::run(&app, &path, &args).await?;
    state.listings.clear();
    let status = cli::run(&app, &path, &vec!["status".to_string()]).await?;
    Ok(parse_status(&status.combined(), true))
}

#[tauri::command]
pub async fn logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let path = state.cli.path_or_err()?;
    cli::run(&app, &path, &vec!["logout".to_string()]).await?;
    state.listings.clear();
    Ok(())
}

/// List a folder, by ID or else by path. Unless `fresh`, a recent listing
/// of the same folder is reused (see `ListingCache`).
async fn fetch_listing(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
    path: Option<&str>,
    fresh: bool,
) -> Result<CachedListing, String> {
    let mut args = vec!["ls".to_string(), "--json".to_string()];
    let key = if let Some(id) = folder_id {
        args.push(id.to_string());
        format!("id:{id}")
    } else {
        let path = normalize_path(path.unwrap_or("/"));
        if path != "/" {
            args.push(path.clone());
        }
        format!("path:{path}")
    };

    let state = app.state::<AppState>();
    if !fresh && let Some(listing) = state.listings.get(&key) {
        return Ok(listing);
    }
    let listing: LsResponse = cli::run_json(app, cli_path, &args).await?;
    state.listings.put(key, listing.clone());
    Ok(CachedListing {
        listing,
        stale: false,
    })
}

/// How long a library sync is reused before checking O2 for new snapshots.
const LIBRARY_SYNC_TTL_MS: i64 = 120_000;
/// The whole-account listing takes about 14 s on a large account.
const ALL_FILES_TTL: Duration = Duration::from_secs(300);
/// After this many own snapshots, publish a full one and drop the deltas.
const SNAPSHOT_COMPACT_AFTER: usize = 8;
/// Leave this much free beyond the original's size when Photos must download it.
const PHOTOS_FREE_SPACE_MARGIN: u64 = 128 * 1024 * 1024;

#[cfg(unix)]
fn free_disk_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0 {
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn free_disk_space(_path: &Path) -> Option<u64> {
    None
}

async fn fetch_all_files(
    app: &AppHandle,
    state: &AppState,
    cli_path: &Path,
    fresh: bool,
) -> Result<Vec<RemoteFile>, String> {
    if !fresh {
        let cache = state.all_files.read().unwrap();
        if let Some(at) = cache.at
            && at.elapsed() < ALL_FILES_TTL
        {
            return Ok(cache.files.clone());
        }
    }
    let args = vec![
        "ls".to_string(),
        "-a".to_string(),
        "--json".to_string(),
    ];
    let files: Vec<RemoteFile> = cli::run_json(app, cli_path, &args).await?;
    let files: Vec<RemoteFile> = files
        .into_iter()
        .filter(|file| {
            let path = file.path.as_deref().unwrap_or("");
            !library::is_internal_path(path) && !library::is_internal_name(&file.name)
        })
        .collect();
    *state.all_files.write().unwrap() = AllFilesCache {
        at: Some(Instant::now()),
        files: files.clone(),
    };
    Ok(files)
}

fn library_is_fresh(state: &AppState) -> bool {
    let library = state.library.read().unwrap();
    !library.pending_snapshot()
        && library
            .last_sync()
            .is_some_and(|last| library::now_ms() - last < LIBRARY_SYNC_TTL_MS)
}

async fn ensure_library_synced(
    app: &AppHandle,
    state: &AppState,
    cli_path: &Path,
    force: bool,
) -> Result<(), String> {
    if !force && library_is_fresh(state) {
        return Ok(());
    }
    let _guard = state.library_sync_lock.lock().await;
    if !force && library_is_fresh(state) {
        return Ok(());
    }
    sync_library_inner(app, state, cli_path).await
}

async fn sync_library_inner(
    app: &AppHandle,
    state: &AppState,
    cli_path: &Path,
) -> Result<(), String> {
    let folder_id = library::ensure_folder(app, cli_path).await?;
    let remote_files = library::list_snapshots(app, cli_path, folder_id).await?;
    let known = state.library.read().unwrap().snapshots().clone();
    let mut downloaded = Vec::new();

    for file in &remote_files {
        let size = file.size.unwrap_or(0);
        if known.get(&file.name).is_some_and(|state| state.id == file.id && state.size == size) {
            continue;
        }
        match library::download_snapshot(app, cli_path, file).await {
            Ok(snapshot) => downloaded.push((
                file.name.clone(),
                library::SnapshotState {
                    id: file.id,
                    size,
                },
                snapshot,
            )),
            Err(_) => {
                // A corrupt or temporarily unavailable snapshot must not stop
                // the rest of the library from loading.
            }
        }
    }

    {
        let mut library = state.library.write().unwrap();
        for (name, snapshot_state, snapshot) in downloaded {
            library.merge_snapshot(snapshot);
            library.remember_snapshot(name, snapshot_state);
        }
        // Snapshots removed by compaction (or from another device) must not
        // stay in the local bookkeeping forever.
        let remote_names: HashSet<String> =
            remote_files.iter().map(|file| file.name.clone()).collect();
        let stale: Vec<String> = library
            .snapshots()
            .keys()
            .filter(|name| !remote_names.contains(*name))
            .cloned()
            .collect();
        for name in stale {
            library.forget_snapshot(&name);
        }
        library.set_folder_id(folder_id);
        library.mark_synced();
        library.save()?;
    }

    if state.library.read().unwrap().pending_snapshot() {
        upload_own_snapshot(app, state, cli_path, folder_id, None).await?;
    }
    Ok(())
}

async fn upload_own_snapshot(
    app: &AppHandle,
    state: &AppState,
    cli_path: &Path,
    folder_id: u64,
    delta: Option<library::LibrarySnapshot>,
) -> Result<(), String> {
    let (full, device_id) = {
        let library = state.library.read().unwrap();
        (library.own_snapshot(), library.device_id().to_string())
    };
    let is_delta = delta.is_some();
    let mut snapshot = delta.unwrap_or_else(|| full.clone());
    if snapshot.entries.is_empty() {
        let mut library = state.library.write().unwrap();
        library.set_folder_id(folder_id);
        library.mark_snapshot_done();
        library.save()?;
        return Ok(());
    }

    // One listing decides both whether the deltas need compacting and which
    // old snapshots to delete if they do.
    let remote_files = library::list_snapshots(app, cli_path, folder_id)
        .await
        .unwrap_or_default();
    let own = library::own_snapshot_files(&remote_files, &device_id);
    let compact = !is_delta || own.len() >= SNAPSHOT_COMPACT_AFTER;
    if compact {
        snapshot = full;
    }

    let (name, reported_id, size) =
        match library::upload_snapshot(app, cli_path, folder_id, &snapshot).await {
            Ok(uploaded) => uploaded,
            Err(error) => {
                let mut library = state.library.write().unwrap();
                library.mark_snapshot_pending();
                let _ = library.save();
                return Err(error);
            }
        };

    let old: Vec<(u64, String)> = if compact {
        own.iter().map(|file| (file.id, file.name.clone())).collect()
    } else {
        Vec::new()
    };
    for (id, _) in &old {
        if *id != reported_id.unwrap_or(0) {
            let _ = library::delete_snapshot(app, cli_path, *id).await;
        }
    }

    let mut library = state.library.write().unwrap();
    library.set_folder_id(folder_id);
    library.remember_snapshot(
        name,
        library::SnapshotState {
            id: reported_id.unwrap_or(0),
            size,
        },
    );
    for (_, old_name) in old {
        library.forget_snapshot(&old_name);
    }
    library.mark_snapshot_done();
    library.save()
}

async fn record_library_uploads(
    app: &AppHandle,
    state: &AppState,
    cli_path: &Path,
    uploads: Vec<NewUpload>,
) -> Result<(), String> {
    if uploads.is_empty() {
        return Ok(());
    }
    // If a previous snapshot failed, publish a full one: a delta would leave
    // the earlier entries unpublished. Otherwise the batch is enough.
    let delta = {
        let library = state.library.read().unwrap();
        if library.pending_snapshot() {
            None
        } else {
            Some(library.snapshot_from_uploads(&uploads))
        }
    };
    {
        let mut library = state.library.write().unwrap();
        library.add_uploads(uploads);
        library.save()?;
    }
    // One snapshot writer at a time: another sync must not delete the file
    // this upload is about to remember.
    let _guard = state.library_sync_lock.lock().await;
    let folder_id = {
        let library = state.library.read().unwrap();
        library.folder_id()
    };
    let folder_id = match folder_id {
        Some(folder_id) => folder_id,
        None => library::ensure_folder(app, cli_path).await?,
    };
    upload_own_snapshot(app, state, cli_path, folder_id, delta).await
}

struct RemoteIndex {
    ids: HashSet<u64>,
    paths: HashSet<String>,
    by_name_size: HashMap<(String, u64), Vec<RemoteFile>>,
}

impl RemoteIndex {
    fn new(files: &[RemoteFile]) -> Self {
        let mut index = Self {
            ids: HashSet::new(),
            paths: HashSet::new(),
            by_name_size: HashMap::new(),
        };
        for file in files {
            index.ids.insert(file.id);
            if let Some(path) = &file.path {
                index.paths.insert(path.clone());
            }
            index
                .by_name_size
                .entry((file.name.clone(), file.size.unwrap_or(0)))
                .or_default()
                .push(file.clone());
        }
        index
    }
}

fn push_copy(copies: &mut Vec<library::LibraryCopyView>, copy: library::LibraryCopyView) {
    let duplicate = copies.iter().any(|existing| {
        match (existing.remote_id, copy.remote_id) {
            (Some(left), Some(right)) => left == right,
            _ => {
                existing.folder_path == copy.folder_path
                    && existing.remote_name == copy.remote_name
            }
        }
    });
    if !duplicate {
        copies.push(copy);
    }
}

fn copies_from_index(index: &RemoteIndex, name: &str, size: u64) -> Vec<library::LibraryCopyView> {
    let mut copies = Vec::new();
    if let Some(files) = index.by_name_size.get(&(name.to_string(), size)) {
        for file in files {
            if let Some(copy) = library::LibraryCopyView::from_remote_file(file) {
                push_copy(&mut copies, copy);
            }
        }
    }
    copies
}

fn prune_library(state: &AppState, index: &RemoteIndex) -> Result<(), String> {
    let mut library = state.library.write().unwrap();
    if library.prune_missing(&index.ids, &index.paths) {
        library.save()?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct PhotoLibraryMatch {
    pub asset_id: String,
    pub sha256: Option<String>,
    pub copies: Vec<library::LibraryCopyView>,
    pub in_target: bool,
}

#[derive(Serialize)]
pub struct PathCheck {
    pub path: String,
    pub sha256: Option<String>,
    pub copies: Vec<library::LibraryCopyView>,
    pub in_target: bool,
    pub is_file: bool,
}

#[tauri::command]
pub fn library_status(state: State<'_, AppState>) -> library::LibraryStats {
    state.library.read().unwrap().stats()
}

#[tauri::command]
pub async fn library_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<library::LibraryStats, String> {
    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    ensure_library_synced(&app, &state, &cli_path, force.unwrap_or(false)).await?;
    Ok(state.library.read().unwrap().stats())
}

#[tauri::command]
pub async fn library_reconcile(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<library::LibraryStats, String> {
    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    ensure_library_synced(&app, &state, &cli_path, true).await?;
    let files = fetch_all_files(&app, &state, &cli_path, true).await?;
    let index = RemoteIndex::new(&files);
    prune_library(&state, &index)?;

    // Publish the cleaned state so other devices don't keep merging copies
    // that no longer exist.
    let _guard = state.library_sync_lock.lock().await;
    let folder_id = {
        let library = state.library.read().unwrap();
        library.folder_id()
    };
    if let Some(folder_id) = folder_id {
        upload_own_snapshot(&app, &state, &cli_path, folder_id, None).await?;
    }
    Ok(state.library.read().unwrap().stats())
}

/// Forget this device's library: delete its snapshots from O2 and clear the
/// local hashes. Snapshots from other devices stay and are merged again on
/// the next sync.
#[tauri::command]
pub async fn library_forget(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<library::LibraryStats, String> {
    let cli_path = state.cli.path_or_err()?;
    let _guard = state.library_sync_lock.lock().await;
    let (folder_id, device_id) = {
        let library = state.library.read().unwrap();
        (library.folder_id(), library.device_id().to_string())
    };

    if let Some(folder_id) = folder_id {
        ensure_session(&app, &cli_path).await?;
        let remote = library::list_snapshots(&app, &cli_path, folder_id).await?;
        for file in library::own_snapshot_files(&remote, &device_id) {
            library::delete_snapshot(&app, &cli_path, file.id).await?;
        }
    }

    library::clear_cached_snapshots(&app)?;
    let mut library = state.library.write().unwrap();
    library.forget();
    library.save()?;
    Ok(library.stats())
}

/// Read old `.o2manifest.json` files and record their whole-file SHA-256 in
/// the shared library, so uploads made before the library existed can match.
#[tauri::command]
pub async fn library_index_manifests(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<library::LibraryStats, String> {
    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    let _guard = state.library_sync_lock.lock().await;
    let files = fetch_all_files(&app, &state, &cli_path, true).await?;
    let cache_root = transfer::app_cache_dir(&app)?.join("manifests");
    std::fs::create_dir_all(&cache_root).map_err(|error| error.to_string())?;

    let mut uploads = Vec::new();
    let mut seen = HashSet::new();
    for file in &files {
        let Some(logical_name) = file.name.strip_suffix(".o2manifest.json") else {
            continue;
        };
        if logical_name.is_empty() {
            continue;
        }
        let folder_path = file
            .path
            .as_deref()
            .and_then(|path| Path::new(path).parent())
            .map(|path| normalize_path(&path.to_string_lossy()))
            .unwrap_or_else(|| "/".into());
        if !seen.insert((folder_path.clone(), logical_name.to_string())) {
            continue;
        }
        if state
            .library
            .read()
            .unwrap()
            .has_copy(&folder_path, logical_name)
        {
            continue;
        }

        let destination = cache_root.join(format!("{}.json", file.id));
        let args = vec![
            "download".to_string(),
            file.id.to_string(),
            "-o".to_string(),
            destination.to_string_lossy().to_string(),
        ];
        if cli::run(&app, &cli_path, &args).await.is_err() {
            continue;
        }
        let raw = match std::fs::read_to_string(&destination) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let _ = std::fs::remove_file(&destination);
        let Ok(manifest) = serde_json::from_str::<ChunkManifest>(&raw) else {
            continue;
        };
        if manifest.sha256.trim().is_empty() || manifest.name.trim().is_empty() {
            continue;
        }
        uploads.push(NewUpload {
            sha256: manifest.sha256,
            size: manifest.size,
            name: manifest.name.clone(),
            captured_at: None,
            source: "manifest".into(),
            folder_id: None,
            folder_path,
            remote_id: None,
            remote_name: manifest.name,
        });
    }

    if !uploads.is_empty() {
        {
            let mut library = state.library.write().unwrap();
            library.add_uploads(uploads);
            library.save()?;
        }
        let folder_id = {
            let library = state.library.read().unwrap();
            library.folder_id()
        };
        let folder_id = match folder_id {
            Some(folder_id) => folder_id,
            None => library::ensure_folder(&app, &cli_path).await?,
        };
        upload_own_snapshot(&app, &state, &cli_path, folder_id, None).await?;
    }

    Ok(state.library.read().unwrap().stats())
}

#[tauri::command]
pub async fn library_match_photos(
    app: AppHandle,
    state: State<'_, AppState>,
    assets: Vec<photos::PhotoAsset>,
    folder_id: Option<u64>,
    folder_path: String,
) -> Result<Vec<PhotoLibraryMatch>, String> {
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    ensure_library_synced(&app, &state, &cli_path, false).await?;
    let files = fetch_all_files(&app, &state, &cli_path, false).await?;
    let index = RemoteIndex::new(&files);
    prune_library(&state, &index)?;

    let target = normalize_path(&folder_path);
    let mut matches = Vec::new();
    for asset in assets {
        let mut copies = Vec::new();
        let mut sha256 = None;
        {
            let library = state.library.read().unwrap();
            for entry in library.lookup_fingerprint(&asset.name, asset.size, asset.date) {
                sha256 = Some(entry.sha256.clone());
                for copy in &entry.copies {
                    push_copy(&mut copies, library::LibraryCopyView::from_copy(copy));
                }
            }
        }
        for copy in copies_from_index(&index, &asset.name, asset.size) {
            push_copy(&mut copies, copy);
        }
        if copies.is_empty() {
            continue;
        }
        let in_target = copies.iter().any(|copy| {
            copy.folder_path == target || folder_id.is_some_and(|id| copy.folder_id == Some(id))
        });
        matches.push(PhotoLibraryMatch {
            asset_id: asset.id,
            sha256,
            copies,
            in_target,
        });
    }
    Ok(matches)
}

#[tauri::command]
pub async fn library_check_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    folder_id: Option<u64>,
    folder_path: String,
) -> Result<Vec<PathCheck>, String> {
    // Folder uploads and non-media files use the normal target-folder check;
    // don't pay for a whole-account listing just to look at them.
    let mut file_flags = Vec::with_capacity(paths.len());
    let mut has_media_file = false;
    for raw_path in &paths {
        let metadata = std::fs::metadata(raw_path).ok();
        let is_file = metadata.as_ref().is_some_and(|meta| meta.is_file());
        if is_file {
            let name = Path::new(raw_path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if camera::media_kind(&name).is_some() {
                has_media_file = true;
            }
        }
        file_flags.push(is_file);
    }
    if !has_media_file {
        return Ok(paths
            .into_iter()
            .zip(file_flags)
            .map(|(path, is_file)| PathCheck {
                path,
                sha256: None,
                copies: Vec::new(),
                in_target: false,
                is_file,
            })
            .collect());
    }

    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    ensure_library_synced(&app, &state, &cli_path, false).await?;
    let files = fetch_all_files(&app, &state, &cli_path, false).await?;
    let index = RemoteIndex::new(&files);
    prune_library(&state, &index)?;

    let target = normalize_path(&folder_path);
    let mut checks = Vec::new();
    for raw_path in paths {
        let path = PathBuf::from(&raw_path);
        let metadata = std::fs::metadata(&path).ok();
        let is_file = metadata.as_ref().is_some_and(|meta| meta.is_file());
        if !is_file {
            checks.push(PathCheck {
                path: raw_path,
                sha256: None,
                copies: Vec::new(),
                in_target: false,
                is_file: false,
            });
            continue;
        }
        let size = metadata.map(|meta| meta.len()).unwrap_or(0);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut copies = Vec::new();
        {
            let library = state.library.read().unwrap();
            for entry in library.lookup_fingerprint(&name, size, None) {
                for copy in &entry.copies {
                    push_copy(&mut copies, library::LibraryCopyView::from_copy(copy));
                }
            }
        }
        for copy in copies_from_index(&index, &name, size) {
            push_copy(&mut copies, copy);
        }

        let mut sha256 = None;
        if copies.is_empty() && camera::media_kind(&name).is_some() {
            // Photos and videos are the bulky duplicates this library is for:
            // hash them so a renamed copy is still recognised.
            if let Ok(hash) = transfer::sha256_file(&path).await {
                if let Some(entry) = state.library.read().unwrap().lookup_hash(&hash) {
                    for copy in &entry.copies {
                        push_copy(&mut copies, library::LibraryCopyView::from_copy(copy));
                    }
                }
                sha256 = Some(hash);
            }
        }
        let in_target = copies.iter().any(|copy| {
            copy.folder_path == target || folder_id.is_some_and(|id| copy.folder_id == Some(id))
        });
        checks.push(PathCheck {
            path: raw_path,
            sha256,
            copies,
            in_target,
            is_file: true,
        });
    }
    Ok(checks)
}

fn year_folders(files: &[LogicalFile]) -> Vec<VirtualFolder> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for file in files {
        let year = camera::year_of(file.date.as_deref());
        let entry = counts.entry(year).or_default();
        entry.0 += 1;
        entry.1 += file.size;
    }

    let mut years: Vec<(String, u64, u64)> = counts
        .into_iter()
        .map(|(year, (count, size))| (year, count, size))
        .collect();
    // Newest first, "Sin fecha" at the end.
    years.sort_by(|a, b| {
        if a.0 == "Sin fecha" {
            std::cmp::Ordering::Greater
        } else if b.0 == "Sin fecha" {
            std::cmp::Ordering::Less
        } else {
            b.0.cmp(&a.0)
        }
    });

    let mut folders = Vec::with_capacity(years.len() + 1);
    folders.push(VirtualFolder {
        key: "camera/all".into(),
        name: "Todo".into(),
        files: files.len() as u64,
        size: files.iter().map(|f| f.size).sum(),
        kind: "all".into(),
    });
    for (year, count, size) in years {
        let key = if year == "Sin fecha" {
            "camera/unknown".to_string()
        } else {
            format!("camera/{year}")
        };
        folders.push(VirtualFolder {
            key,
            name: year,
            files: count,
            size,
            kind: "year".into(),
        });
    }
    folders
}

#[tauri::command]
pub async fn list_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: Option<u64>,
    path: Option<String>,
    virtual_path: Option<String>,
    fresh: Option<bool>,
) -> Result<LogicalListing, String> {
    let cli_path = state.cli.path_or_err()?;
    let settings = state.settings.read().unwrap().clone();

    // Virtual views always work on the root listing; nothing is moved in O2.
    let (base_folder, base_path) = if virtual_path.is_some() {
        (None, Some("/".to_string()))
    } else {
        (folder_id, path.clone())
    };
    let CachedListing {
        mut listing,
        stale,
    } = fetch_listing(
        &app,
        &cli_path,
        base_folder,
        base_path.as_deref(),
        fresh.unwrap_or(false),
    )
    .await?;
    // The shared upload library lives in a hidden folder in O2; keep it out
    // of the Finder-style listing and the virtual camera view.
    listing
        .folders
        .retain(|folder| !library::is_internal_name(&folder.name));
    listing
        .files
        .retain(|file| !library::is_internal_name(&file.name));
    let (mut files, mut warnings) = transfer::group_logical_files(listing.files);
    let mut virtual_folders = Vec::new();

    match virtual_path.as_deref() {
        Some("camera") => {
            files.retain(camera::is_camera_capture);
            virtual_folders = year_folders(&files);
            files.clear();
        }
        Some(key) if key.starts_with("camera/") => {
            files.retain(camera::is_camera_capture);
            let selected = key.trim_start_matches("camera/");
            if selected != "all" {
                let year = if selected == "unknown" {
                    "Sin fecha".to_string()
                } else {
                    selected.to_string()
                };
                files.retain(|f| camera::year_of(f.date.as_deref()) == year);
            }
        }
        _ => {
            let at_root = base_folder.is_none()
                && base_path
                    .as_deref()
                    .map(normalize_path)
                    .as_deref()
                    .map(|p| p == "/")
                    .unwrap_or(true);
            if at_root && settings.group_camera {
                let (camera_files, other): (Vec<_>, Vec<_>) =
                    files.into_iter().partition(camera::is_camera_capture);
                if !camera_files.is_empty() {
                    virtual_folders.push(VirtualFolder {
                        key: "camera".into(),
                        name: "Cámara".into(),
                        files: camera_files.len() as u64,
                        size: camera_files.iter().map(|f| f.size).sum(),
                        kind: "camera".into(),
                    });
                }
                files = other;
            }
        }
    }

    if virtual_path.is_some() {
        warnings.clear();
    }

    Ok(LogicalListing {
        path: if listing.path.is_empty() {
            path.unwrap_or_else(|| "/".into())
        } else {
            listing.path
        },
        folders: if virtual_path.is_some() {
            Vec::new()
        } else {
            listing.folders
        },
        files,
        warnings,
        virtual_folders,
        stale,
    })
}

/// Create `name` in `parent_path`, or find it if it already exists. The
/// flag says whether it was created (so it is known to be empty).
async fn create_folder_inner(
    app: &AppHandle,
    cli_path: &Path,
    parent_path: &str,
    name: &str,
) -> Result<(RemoteFolder, bool), String> {
    let parent_path = normalize_path(parent_path);
    let target = join_path(&parent_path, name);
    // `-p` reports an existing folder (with its id) instead of failing.
    let args = vec!["mkdir".to_string(), "-p".to_string(), target.clone()];
    if let Ok(out) = cli::run(app, cli_path, &args).await {
        let out = out.combined();
        if let Some(id) = transfer::parse_created_id(&out) {
            let folder = RemoteFolder {
                id,
                name: name.to_string(),
                files: 0,
                size: 0,
            };
            return Ok((folder, !out.contains("already exists")));
        }
    }

    // The CLI did not print an id; the folder may exist anyway.
    let listing = fetch_listing(app, cli_path, None, Some(&parent_path), true).await?;
    if let Some(folder) = listing
        .listing
        .folders
        .into_iter()
        .find(|f| f.name == name)
    {
        return Ok((folder, false));
    }
    Err(format!("No se pudo crear la carpeta {target}"))
}

#[derive(Serialize)]
pub struct CreatedFolder {
    pub folder: RemoteFolder,
    /// False when the folder already existed, so it may not be empty.
    pub created: bool,
}

#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    parent_id: Option<u64>,
    parent_path: String,
    name: String,
) -> Result<CreatedFolder, String> {
    let cli_path = state.cli.path_or_err()?;
    ensure_session(&app, &cli_path).await?;
    let (folder, created) =
        create_folder_inner(&app, &cli_path, &parent_path, &name).await?;
    let parent_path = normalize_path(&parent_path);

    // A folder created by this command is empty, so opening it does not need
    // another whole-account `ls` (up to a minute on a large account).
    let folder_path = join_path(&parent_path, &name);
    if created {
        let empty = LsResponse {
            path: folder_path.clone(),
            folders: Vec::new(),
            files: Vec::new(),
        };
        state
            .listings
            .put(format!("id:{}", folder.id), empty.clone());
        state
            .listings
            .put(format!("path:{folder_path}"), empty);
    }

    // Update the parent's cached listing instead of clearing every listing:
    // a new folder only changes the folder it was created in.
    let mut parent_keys = vec![format!("path:{parent_path}")];
    if let Some(id) = parent_id {
        parent_keys.push(format!("id:{id}"));
    }
    for key in parent_keys {
        state.listings.update(&key, |listing| {
            if !listing
                .folders
                .iter()
                .any(|existing| existing.id == folder.id)
            {
                listing.folders.push(folder.clone());
            }
        });
    }

    Ok(CreatedFolder { folder, created })
}

#[derive(Serialize)]
pub struct UploadSummary {
    pub uploaded: u64,
    /// Already in O2 with the same name and size, so not uploaded again.
    pub skipped: u64,
    pub failed: Vec<String>,
    /// The uploads succeeded but the shared library could not be updated.
    pub library_warning: Option<String>,
}

impl UploadSummary {
    fn add(&mut self, outcome: &UploadOutcome) {
        match outcome {
            UploadOutcome::Uploaded => self.uploaded += 1,
            UploadOutcome::Skipped => self.skipped += 1,
        }
    }
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// A remote folder being uploaded into. `existing` is filled the first
/// time a file goes into it; a folder this upload created starts empty.
/// `logical` groups completed chunks under their logical file name, so a
/// Photos item can be skipped before exporting it from iCloud.
struct UploadTarget {
    id: Option<u64>,
    path: String,
    existing: Option<ExistingFiles>,
    logical: Option<ExistingFiles>,
}

impl UploadTarget {
    async fn existing(&mut self, app: &AppHandle, cli_path: &Path) -> &ExistingFiles {
        if self.existing.is_none() {
            // A stale disk listing can miss files uploaded elsewhere; ask O2
            // before deciding whether a file is already in the target folder.
            let mut listing = fetch_listing(app, cli_path, self.id, Some(&self.path), false).await;
            if listing.as_ref().is_ok_and(|listing| listing.stale) {
                listing = fetch_listing(app, cli_path, self.id, Some(&self.path), true).await;
            }
            // Without a listing nothing is skipped; the upload still goes.
            let files = listing
                .map(|listing| listing.listing.files)
                .unwrap_or_default();
            self.logical = Some(transfer::logical_files_existing(&files));
            self.existing = Some(transfer::existing_files(&files));
        }
        self.existing.as_ref().unwrap()
    }

    fn has_logical(&self, name: &str, size: u64) -> bool {
        self.logical
            .as_ref()
            .is_some_and(|existing| transfer::already_uploaded(existing, name, size))
    }
}

fn new_upload_from_receipt(
    receipt: &transfer::UploadReceipt,
    name: String,
    captured_at: Option<i64>,
    source: &str,
    folder_id: Option<u64>,
    folder_path: String,
) -> Option<NewUpload> {
    if !matches!(receipt.outcome, UploadOutcome::Uploaded) {
        return None;
    }
    Some(NewUpload {
        sha256: receipt.sha256.clone()?,
        size: receipt.size,
        name,
        captured_at,
        source: source.to_string(),
        folder_id,
        folder_path,
        remote_id: receipt.remote_id,
        remote_name: receipt.remote_name.clone(),
    })
}

fn library_copy_folder(state: &AppState, sha256: &str) -> Option<String> {
    state
        .library
        .read()
        .unwrap()
        .lookup_hash(sha256)
        .and_then(|entry| entry.copies.first().map(|copy| copy.folder_path.clone()))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn upload_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    folder_id: Option<u64>,
    folder_path: String,
    transfer_ids: Option<Vec<String>>,
    known_hashes: Option<Vec<Option<String>>>,
    skip_known: Option<bool>,
) -> Result<UploadSummary, String> {
    let cli_path = state.cli.path_or_err()?;
    // The frontend already shows each path as queued under these IDs, so
    // the rows are there while this waits for earlier uploads.
    let transfer_ids = transfer_ids.unwrap_or_default();
    ensure_session(&app, &cli_path).await?;
    let _one_at_a_time = state.upload_lock.lock().await;
    let settings = state.settings.read().unwrap().clone();
    let chunk_size = settings.chunk_size_mb * 1024 * 1024;
    let mut summary = UploadSummary {
        uploaded: 0,
        skipped: 0,
        failed: Vec::new(),
        library_warning: None,
    };
    let mut library_uploads = Vec::new();
    let skip_known = skip_known.unwrap_or(false);

    let mut target = UploadTarget {
        id: folder_id,
        path: normalize_path(&folder_path),
        existing: None,
        logical: None,
    };

    for (index, raw_path) in paths.into_iter().enumerate() {
        let known_sha256 = known_hashes
            .as_ref()
            .and_then(|hashes| hashes.get(index))
            .and_then(|hash| hash.as_deref());
        let transfer_id = transfer_ids
            .get(index)
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let path = PathBuf::from(&raw_path);
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| raw_path.clone());
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                transfer::emit_failed(&app, &transfer_id, "upload", &display_name, &err.to_string());
                summary.failed.push(format!("{raw_path}: {err}"));
                continue;
            }
        };

        if meta.is_file() {
            if skip_known
                && let Some(hash) = known_sha256
                && let Some(folder) = library_copy_folder(&state, hash)
            {
                let size = meta.len();
                let _ = app.emit(
                    "transfer-progress",
                    TransferProgress {
                        id: transfer_id.clone(),
                        kind: "upload".into(),
                        name: display_name.clone(),
                        phase: "done".into(),
                        current: 1,
                        total: 1,
                        bytes_done: size,
                        bytes_total: size,
                        message: format!("Ya estaba en O2 en {folder}; no se vuelve a subir"),
                    },
                );
                summary.skipped += 1;
                continue;
            }
            let existing = target.existing(&app, &cli_path).await;
            match transfer::upload_one(
                &app,
                &cli_path,
                &path,
                folder_id,
                chunk_size,
                &transfer_id,
                existing,
                known_sha256,
            )
            .await
            {
                Ok(receipt) => {
                    summary.add(&receipt.outcome);
                    if let Some(upload) = new_upload_from_receipt(
                        &receipt,
                        display_name,
                        None,
                        "file",
                        folder_id,
                        target.path.clone(),
                    ) {
                        library_uploads.push(upload);
                    }
                }
                Err(err) => summary.failed.push(format!("{raw_path}: {err}")),
            }
            continue;
        }

        if !meta.is_dir() {
            let err = "no es un archivo ni una carpeta";
            transfer::emit_failed(&app, &transfer_id, "upload", &display_name, err);
            summary.failed.push(format!("{raw_path}: {err}"));
            continue;
        }

        let root_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("carpeta")
            .to_string();
        let folder_transfer_id = transfer_id;
        let files_in_folder = WalkDir::new(&path)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .count() as u64;
        let mut folder_done = 0u64;
        let folder_progress = |done: u64, phase: &str, message: String| {
            let _ = app.emit(
                "transfer-progress",
                TransferProgress {
                    id: folder_transfer_id.clone(),
                    kind: "upload".into(),
                    name: root_name.clone(),
                    phase: phase.into(),
                    current: done,
                    total: files_in_folder,
                    bytes_done: 0,
                    bytes_total: 0,
                    message,
                },
            );
        };
        folder_progress(0, "scan", format!("0 de {files_in_folder} archivos"));
        let failed_before = summary.failed.len();
        let (root_folder, created) =
            match create_folder_inner(&app, &cli_path, &target.path, &root_name).await {
                Ok(found) => found,
                Err(err) => {
                    folder_progress(0, "error", err.clone());
                    summary.failed.push(format!("{raw_path}: {err}"));
                    continue;
                }
            };

        let mut remote: HashMap<PathBuf, UploadTarget> = HashMap::new();
        remote.insert(
            path.clone(),
            UploadTarget {
                id: Some(root_folder.id),
                path: join_path(&target.path, &root_name),
                existing: created.then(ExistingFiles::new),
                logical: created.then(ExistingFiles::new),
            },
        );

        for entry in WalkDir::new(&path)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    summary.failed.push(format!("{raw_path}: {err}"));
                    continue;
                }
            };
            let local = entry.path().to_path_buf();
            let Some(parent) = local.parent() else {
                continue;
            };
            let Some(parent_target) = remote.get_mut(parent) else {
                continue;
            };

            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let parent_path = parent_target.path.clone();
                match create_folder_inner(&app, &cli_path, &parent_path, &name).await {
                    Ok((folder, created)) => {
                        remote.insert(
                            local,
                            UploadTarget {
                                id: Some(folder.id),
                                path: join_path(&parent_path, &name),
                                existing: created.then(ExistingFiles::new),
                                logical: created.then(ExistingFiles::new),
                            },
                        );
                    }
                    Err(err) => summary.failed.push(format!("{}: {err}", local.display())),
                }
            } else if entry.file_type().is_file() {
                // Named after the folder's, so the frontend shows it under it.
                let transfer_id = format!("{folder_transfer_id}/{}", uuid::Uuid::new_v4());
                let parent_id = parent_target.id;
                let parent_path = parent_target.path.clone();
                let existing = parent_target.existing(&app, &cli_path).await;
                match transfer::upload_one(
                    &app,
                    &cli_path,
                    &local,
                    parent_id,
                    chunk_size,
                    &transfer_id,
                    existing,
                    None,
                )
                .await
                {
                    Ok(receipt) => {
                        summary.add(&receipt.outcome);
                        if let Some(upload) = new_upload_from_receipt(
                            &receipt,
                            local
                                .file_name()
                                .map(|name| name.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            None,
                            "file",
                            parent_id,
                            parent_path,
                        ) {
                            library_uploads.push(upload);
                        }
                    }
                    Err(err) => summary.failed.push(format!("{}: {err}", local.display())),
                }
                folder_done += 1;
                folder_progress(
                    folder_done,
                    "upload",
                    format!("{folder_done} de {files_in_folder} archivos"),
                );
            }
        }

        let folder_failed = summary.failed.len() - failed_before;
        let (phase, message) = if folder_failed == 0 {
            ("done", format!("Carpeta completada ({folder_done} archivos)"))
        } else {
            (
                "error",
                format!("{folder_failed} de {files_in_folder} archivos han fallado"),
            )
        };
        folder_progress(folder_done, phase, message);
    }

    if let Err(error) =
        record_library_uploads(&app, &state, &cli_path, library_uploads).await
    {
        summary.library_warning = Some(format!(
            "Los archivos se han subido, pero no se ha podido actualizar la biblioteca: {error}"
        ));
    }
    state.listings.clear();
    Ok(summary)
}

#[derive(Serialize)]
pub struct PhotosAccess {
    pub available: bool,
    pub status: String,
}

#[tauri::command]
pub fn photos_access_status() -> PhotosAccess {
    PhotosAccess {
        available: photos::available(),
        status: photos::authorization_status(),
    }
}

#[tauri::command]
pub async fn photos_request_access() -> Result<PhotosAccess, String> {
    let status = photos::request_access().await?;
    Ok(PhotosAccess {
        available: photos::available(),
        status,
    })
}

#[tauri::command]
pub async fn photos_list(
    media_type: Option<String>,
    min_size: Option<u64>,
) -> Result<photos::PhotoList, String> {
    photos::list(
        media_type.as_deref().unwrap_or("video"),
        min_size.unwrap_or(0),
    )
    .await
}

fn safe_component(value: &str) -> String {
    let component: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if component.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        component
    }
}

/// Export selected Photos originals (including iCloud-only ones) to the
/// app cache and upload them through the same queue and chunking as local
/// files. Files already in the target folder with the same name and size
/// are skipped before downloading them from iCloud.
#[tauri::command]
pub async fn upload_photos(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    folder_id: Option<u64>,
    folder_path: String,
    transfer_ids: Option<Vec<String>>,
    skip_known: Option<bool>,
) -> Result<UploadSummary, String> {
    if !photos::available() {
        return Err("Subir desde Fotos solo está disponible en macOS.".into());
    }
    let cli_path = state.cli.path_or_err()?;
    let transfer_ids = transfer_ids.unwrap_or_default();
    let _one_at_a_time = state.upload_lock.lock().await;
    // Exporting an iCloud original can take a long time; don't start it if
    // the O2 session is already gone.
    ensure_session(&app, &cli_path).await?;
    // Use the latest shared snapshots, but never block an upload when the
    // library cannot be reached.
    let _ = ensure_library_synced(&app, &state, &cli_path, false).await;
    let settings = state.settings.read().unwrap().clone();
    let chunk_size = settings.chunk_size_mb * 1024 * 1024;
    let mut summary = UploadSummary {
        uploaded: 0,
        skipped: 0,
        failed: Vec::new(),
        library_warning: None,
    };
    let mut library_uploads = Vec::new();
    let skip_known = skip_known.unwrap_or(false);
    let mut target = UploadTarget {
        id: folder_id,
        path: normalize_path(&folder_path),
        existing: None,
        logical: None,
    };
    let cache_root = transfer::app_cache_dir(&app)?.join("photos");

    for (index, asset_id) in asset_ids.into_iter().enumerate() {
        let transfer_id = transfer_ids
            .get(index)
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let asset = match photos::asset(&asset_id).await {
            Ok(asset) => asset,
            Err(err) => {
                transfer::emit_failed(&app, &transfer_id, "upload", &asset_id, &err);
                summary.failed.push(format!("{asset_id}: {err}"));
                continue;
            }
        };
        let name = photos::safe_file_name(&asset.name);

        if asset.size > 0 {
            let already_uploaded = {
                let existing = target.existing(&app, &cli_path).await;
                transfer::already_uploaded(existing, &name, asset.size)
            };
            if already_uploaded || target.has_logical(&name, asset.size) {
                let _ = app.emit(
                    "transfer-progress",
                    TransferProgress {
                        id: transfer_id.clone(),
                        kind: "upload".into(),
                        name: name.clone(),
                        phase: "done".into(),
                        current: 1,
                        total: 1,
                        bytes_done: asset.size,
                        bytes_total: asset.size,
                        message: "Ya estaba en O2; no se vuelve a subir".into(),
                    },
                );
                summary.skipped += 1;
                continue;
            }
        }

        if asset.size > 0 {
            let needed = asset.size.saturating_add(PHOTOS_FREE_SPACE_MARGIN);
            if let Some(free) = free_disk_space(cache_root.parent().unwrap_or(&cache_root))
                && free < needed
            {
                let message = format!(
                    "Espacio libre insuficiente: se necesitan {} MB y quedan {} MB",
                    needed / (1024 * 1024),
                    free / (1024 * 1024)
                );
                transfer::emit_failed(&app, &transfer_id, "upload", &name, &message);
                summary.failed.push(format!("{name}: {message}"));
                continue;
            }
        }

        let temp_root = cache_root.join(safe_component(&transfer_id));
        if let Err(err) = tokio::fs::create_dir_all(&temp_root).await {
            transfer::emit_failed(&app, &transfer_id, "upload", &name, &err.to_string());
            summary.failed.push(format!("{name}: {err}"));
            continue;
        }
        let destination = temp_root.join(&name);

        if let Err(err) =
            photos::export(app.clone(), transfer_id.clone(), asset.clone(), destination.clone())
                .await
        {
            transfer::emit_failed(&app, &transfer_id, "upload", &name, &err);
            summary.failed.push(format!("{name}: {err}"));
            let _ = tokio::fs::remove_dir_all(&temp_root).await;
            continue;
        }

        let sha256 = transfer::sha256_file(&destination).await.ok();
        if skip_known
            && let Some(hash) = &sha256
            && let Some(folder) = library_copy_folder(&state, hash)
        {
            let _ = app.emit(
                "transfer-progress",
                TransferProgress {
                    id: transfer_id.clone(),
                    kind: "upload".into(),
                    name: name.clone(),
                    phase: "done".into(),
                    current: 1,
                    total: 1,
                    bytes_done: asset.size,
                    bytes_total: asset.size,
                    message: format!("Ya estaba en O2 en {folder}; no se vuelve a subir"),
                },
            );
            summary.skipped += 1;
            let _ = tokio::fs::remove_dir_all(&temp_root).await;
            continue;
        }

        let existing = target.existing(&app, &cli_path).await;
        match transfer::upload_one(
            &app,
            &cli_path,
            &destination,
            folder_id,
            chunk_size,
            &transfer_id,
            existing,
            sha256.as_deref(),
        )
        .await
        {
            Ok(receipt) => {
                summary.add(&receipt.outcome);
                if let Some(upload) = new_upload_from_receipt(
                    &receipt,
                    asset.name.clone(),
                    asset.date,
                    "photos",
                    folder_id,
                    target.path.clone(),
                ) {
                    library_uploads.push(upload);
                }
            }
            Err(err) => summary.failed.push(format!("{name}: {err}")),
        }
        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }

    if let Err(error) =
        record_library_uploads(&app, &state, &cli_path, library_uploads).await
    {
        summary.library_warning = Some(format!(
            "Los archivos se han subido, pero no se ha podido actualizar la biblioteca: {error}"
        ));
    }
    state.listings.clear();
    Ok(summary)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn download_entry(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    name: String,
    dest: String,
    chunked: bool,
    manifest_id: Option<u64>,
    parts: Option<Vec<PartRef>>,
    size: Option<u64>,
    folder_id: Option<u64>,
    overwrite: Option<bool>,
    transfer_id: Option<String>,
) -> Result<(), String> {
    let cli_path = state.cli.path_or_err()?;
    let dest = PathBuf::from(dest);
    // The frontend shows the download as queued under this ID beforehand.
    let transfer_id = transfer_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // The save dialog asks before replacing; downloads into a chosen folder
    // never replace anything.
    let result = if !overwrite.unwrap_or(false) && dest.exists() {
        Err(format!("{} ya existe en esa carpeta", dest.display()))
    } else if chunked {
        match manifest_id {
            Some(manifest_id) => {
                transfer::download_chunked(
                    &app,
                    &cli_path,
                    folder_id,
                    manifest_id,
                    &parts.unwrap_or_default(),
                    &name,
                    &dest,
                    &transfer_id,
                )
                .await
            }
            None => Err("Este archivo no tiene manifiesto".to_string()),
        }
    } else {
        transfer::download_normal(&app, &cli_path, id, size, &dest, &transfer_id, &name).await
    };
    if let Err(err) = &result {
        transfer::emit_failed(&app, &transfer_id, "download", &name, err);
    }
    result
}

async fn rm(app: &AppHandle, cli_path: &Path, id: u64, recursive: bool) -> Result<(), String> {
    let mut args = vec!["rm".to_string(), id.to_string()];
    if recursive {
        args.push("-r".into());
        args.push("-y".into());
    }
    cli::run(app, cli_path, &args).await.map(|_| ())
}

/// A file to move to the trash: its ID, or a chunked file's part IDs and
/// manifest.
#[derive(Deserialize)]
pub struct FileToDelete {
    ids: Vec<u64>,
    manifest_id: Option<u64>,
}

/// Move files and folders to the O2 trash. Files go a few at a time; a
/// chunked file's manifest only after all its parts, so if a part fails the
/// file stays listed and the delete can be retried. Folders go last.
#[tauri::command]
pub async fn delete_entries(
    app: AppHandle,
    state: State<'_, AppState>,
    files: Vec<FileToDelete>,
    folders: Vec<u64>,
) -> Result<(), String> {
    let cli_path = state.cli.path_or_err()?;
    let file_ids: HashSet<u64> = files
        .iter()
        .flat_map(|file| file.ids.iter().copied().chain(file.manifest_id))
        .collect();
    let folder_ids: HashSet<u64> = folders.iter().copied().collect();
    let result = delete_entries_inner(&app, &cli_path, files, folders).await;
    if result.is_ok() {
        // Keep the other cached folders: only the deleted entries changed.
        state.listings.remove_entries(&file_ids, &folder_ids);
    } else {
        // A partial delete: don't guess which entries survived.
        state.listings.clear();
    }
    state.all_files.write().unwrap().at = None;
    result
}

async fn delete_entries_inner(
    app: &AppHandle,
    cli_path: &Path,
    files: Vec<FileToDelete>,
    folders: Vec<u64>,
) -> Result<(), String> {
    // With an expired session, `rm` would open a login window and wait.
    ensure_session(app, cli_path).await?;

    let mut errors = Vec::new();
    let ids = files.iter().flat_map(|f| f.ids.iter().copied()).collect();
    let failed = rm_parallel(app, cli_path, ids, &mut errors).await;
    let manifests = match failed {
        Some(failed) => files
            .iter()
            .filter(|f| f.ids.iter().all(|id| !failed.contains(id)))
            .filter_map(|f| f.manifest_id)
            .collect(),
        // A task died without saying which part: keep every manifest.
        None => Vec::new(),
    };
    rm_parallel(app, cli_path, manifests, &mut errors).await;
    for id in folders {
        if let Err(err) = rm(app, cli_path, id, true).await {
            errors.push(err);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// `rm` each of `ids`, `PARALLEL_DELETES` at a time (each `rm` lists the
/// whole account first). Errors go to `errors`; returns the IDs that
/// failed, or `None` if a task died and the failures aren't all known.
async fn rm_parallel(
    app: &AppHandle,
    cli_path: &Path,
    ids: Vec<u64>,
    errors: &mut Vec<String>,
) -> Option<HashSet<u64>> {
    let semaphore = Arc::new(Semaphore::new(PARALLEL_DELETES));
    let mut tasks = JoinSet::new();
    for id in ids {
        let Ok(permit) = semaphore.clone().acquire_owned().await else {
            break;
        };
        let app = app.clone();
        let cli_path = cli_path.to_path_buf();
        tasks.spawn(async move {
            let _permit = permit;
            (id, rm(&app, &cli_path, id, false).await)
        });
    }

    let mut failed = HashSet::new();
    let mut all_known = true;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((_, Ok(()))) => {}
            Ok((id, Err(err))) => {
                failed.insert(id);
                errors.push(err);
            }
            Err(err) => {
                all_known = false;
                errors.push(err.to_string());
            }
        }
    }
    all_known.then_some(failed)
}

#[tauri::command]
pub async fn get_quota(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let cli_path = state.cli.path_or_err()?;
    let output = cli::run(&app, &cli_path, &vec!["quota".to_string()]).await?;
    Ok(output.text().to_string())
}

#[tauri::command]
pub async fn check_cli_update(state: State<'_, AppState>) -> Result<CliRelease, String> {
    let info = state.cli.info();
    cli::check_update(&info.version).await
}

#[tauri::command]
pub async fn install_cli_update(
    app: AppHandle,
    state: State<'_, AppState>,
    version: String,
    url: String,
) -> Result<CliInfo, String> {
    let info = cli::download_and_install(&app, &version, &url).await?;
    let new_path = PathBuf::from(&info.path);

    // Try the new CLI against this account before switching to it: a bad
    // release must not leave the app unable to list files. Without an
    // active session the listing can't be tried (it would open the login
    // window); the next login exercises it.
    let rejected = |err: String| {
        format!("La versión {version} no ha pasado la prueba; se mantiene la anterior. {err}")
    };
    if session_active(&app, &new_path).await.map_err(rejected)? {
        let test_args = vec!["ls".to_string(), "--json".to_string()];
        cli::run_json::<LsResponse>(&app, &new_path, &test_args)
            .await
            .map_err(rejected)?;
    }

    cli::write_current(&app, &new_path, &info.version)?;
    state
        .cli
        .set(new_path, info.version.clone(), info.source.clone());
    state.listings.clear();
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, date: Option<&str>, size: u64) -> LogicalFile {
        LogicalFile {
            name: name.into(),
            size,
            date: date.map(|s| s.to_string()),
            chunked: false,
            manifest_id: None,
            normal_id: Some(1),
            parts: vec![],
            incomplete: false,
        }
    }

    #[test]
    fn camera_years_are_sorted_newest_first_with_unknown_last() {
        let files = vec![
            file("IMG_2026.jpg", Some("2026-01-01T00:00:00Z"), 10),
            file("IMG_2025.jpg", Some("2025-01-01T00:00:00Z"), 20),
            file("IMG_2025b.jpg", Some("2025-02-01T00:00:00Z"), 30),
            file("old.jpg", None, 5),
        ];
        let folders = year_folders(&files);
        assert_eq!(folders[0].key, "camera/all");
        assert_eq!(folders[0].files, 4);
        assert_eq!(folders[0].size, 65);
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Todo", "2026", "2025", "Sin fecha"]);
        assert_eq!(folders[2].files, 2);
        assert_eq!(folders[2].size, 50);
        assert_eq!(folders[3].key, "camera/unknown");
    }

    #[test]
    fn session_is_active_only_when_o2_accepts_it() {
        let active = "✓ Logged in\n  Phone: +34 600000000\n  Session: active\n  Storage: 1 GB of 10 TB used\n";
        let status = parse_status(active, true);
        assert!(status.logged_in);
        assert_eq!(status.storage.as_deref(), Some("1 GB of 10 TB used"));

        let expired = "✓ Logged in\n  Session: expired (the next command opens the login window)\n";
        assert!(!parse_status(expired, true).logged_in);
        assert!(!parse_status("✗ Not logged in. Run `o2cloud login` to authenticate.\n", true).logged_in);
    }
}
