use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use walkdir::WalkDir;

use crate::cache::ListingCache;
use crate::camera;
use crate::cli::{self, CliManager};
use crate::transfer::{self, ExistingFiles, UploadOutcome};
use crate::types::{
    AppSettings, CliInfo, CliRelease, LogicalFile, LsResponse, LogicalListing, PartRef,
    RemoteFolder, SessionStatus, TransferProgress, VirtualFolder,
};

/// Each `rm` lists the whole account before deleting; a few at a time keeps
/// a chunked file's delete short without flooding O2.
const PARALLEL_DELETES: usize = 3;

/// The frontend opens the login window when it sees this.
const SESSION_EXPIRED: &str = "La sesión de O2 ha caducado. Vuelve a iniciar sesión.";

pub struct AppState {
    pub cli: CliManager,
    pub settings: RwLock<AppSettings>,
    listings: ListingCache,
    /// Uploads run one at a time, in the order they were started.
    upload_lock: Mutex<()>,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Self {
        let cli = CliManager::discover(app);
        let settings = load_settings(app);
        Self {
            cli,
            settings: RwLock::new(settings),
            listings: ListingCache::default(),
            upload_lock: Mutex::new(()),
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
) -> Result<LsResponse, String> {
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
    Ok(listing)
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
    let listing = fetch_listing(
        &app,
        &cli_path,
        base_folder,
        base_path.as_deref(),
        fresh.unwrap_or(false),
    )
    .await?;
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
    if let Some(folder) = listing.folders.into_iter().find(|f| f.name == name) {
        return Ok((folder, false));
    }
    Err(format!("No se pudo crear la carpeta {target}"))
}

#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    parent_path: String,
    name: String,
) -> Result<RemoteFolder, String> {
    let cli_path = state.cli.path_or_err()?;
    let result = create_folder_inner(&app, &cli_path, &parent_path, &name).await;
    state.listings.clear();
    result.map(|(folder, _)| folder)
}

#[derive(Serialize)]
pub struct UploadSummary {
    pub uploaded: u64,
    /// Already in O2 with the same name and size, so not uploaded again.
    pub skipped: u64,
    pub failed: Vec<String>,
}

impl UploadSummary {
    fn add(&mut self, outcome: UploadOutcome) {
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
struct UploadTarget {
    id: Option<u64>,
    path: String,
    existing: Option<ExistingFiles>,
}

impl UploadTarget {
    async fn existing(&mut self, app: &AppHandle, cli_path: &Path) -> &ExistingFiles {
        if self.existing.is_none() {
            // Without a listing nothing is skipped; the upload still goes.
            let files = fetch_listing(app, cli_path, self.id, Some(&self.path), false)
                .await
                .map(|listing| listing.files)
                .unwrap_or_default();
            self.existing = Some(transfer::existing_files(&files));
        }
        self.existing.as_ref().unwrap()
    }
}

#[tauri::command]
pub async fn upload_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    folder_id: Option<u64>,
    folder_path: String,
) -> Result<UploadSummary, String> {
    let cli_path = state.cli.path_or_err()?;
    let _one_at_a_time = state.upload_lock.lock().await;
    let settings = state.settings.read().unwrap().clone();
    let chunk_size = settings.chunk_size_mb * 1024 * 1024;
    let mut summary = UploadSummary {
        uploaded: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    let mut target = UploadTarget {
        id: folder_id,
        path: normalize_path(&folder_path),
        existing: None,
    };

    for raw_path in paths {
        let path = PathBuf::from(&raw_path);
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                summary.failed.push(format!("{raw_path}: {err}"));
                continue;
            }
        };

        if meta.is_file() {
            let transfer_id = uuid::Uuid::new_v4().to_string();
            let existing = target.existing(&app, &cli_path).await;
            match transfer::upload_one(
                &app,
                &cli_path,
                &path,
                folder_id,
                chunk_size,
                &transfer_id,
                existing,
            )
            .await
            {
                Ok(outcome) => summary.add(outcome),
                Err(err) => summary.failed.push(format!("{raw_path}: {err}")),
            }
            continue;
        }

        if !meta.is_dir() {
            summary
                .failed
                .push(format!("{raw_path}: no es un archivo ni una carpeta"));
            continue;
        }

        let root_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("carpeta")
            .to_string();
        let folder_transfer_id = uuid::Uuid::new_v4().to_string();
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
                            },
                        );
                    }
                    Err(err) => summary.failed.push(format!("{}: {err}", local.display())),
                }
            } else if entry.file_type().is_file() {
                let transfer_id = uuid::Uuid::new_v4().to_string();
                let parent_id = parent_target.id;
                let existing = parent_target.existing(&app, &cli_path).await;
                match transfer::upload_one(
                    &app,
                    &cli_path,
                    &local,
                    parent_id,
                    chunk_size,
                    &transfer_id,
                    existing,
                )
                .await
                {
                    Ok(outcome) => summary.add(outcome),
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
) -> Result<(), String> {
    let cli_path = state.cli.path_or_err()?;
    let dest = PathBuf::from(dest);
    let transfer_id = uuid::Uuid::new_v4().to_string();
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
    let result = delete_entries_inner(&app, &cli_path, files, folders).await;
    state.listings.clear();
    result
}

async fn delete_entries_inner(
    app: &AppHandle,
    cli_path: &Path,
    files: Vec<FileToDelete>,
    folders: Vec<u64>,
) -> Result<(), String> {
    let commands: usize = files
        .iter()
        .map(|f| f.ids.len() + usize::from(f.manifest_id.is_some()))
        .sum::<usize>()
        + folders.len();
    // With an expired session, each parallel `rm` would open a login window.
    if commands > 1 && !session_active(app, cli_path).await? {
        return Err(SESSION_EXPIRED.into());
    }

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
