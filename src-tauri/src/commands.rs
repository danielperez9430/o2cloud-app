use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use walkdir::WalkDir;

use crate::camera;
use crate::cli::{self, CliManager};
use crate::transfer;
use crate::types::{
    AppSettings, CliInfo, CliRelease, LogicalFile, LsResponse, LogicalListing, RemoteFolder,
    SessionStatus, TransferProgress, VirtualFolder,
};

pub struct AppState {
    pub cli: CliManager,
    pub settings: RwLock<AppSettings>,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Self {
        let cli = CliManager::discover(app);
        let settings = load_settings(app);
        Self {
            cli,
            settings: RwLock::new(settings),
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
    settings.concurrency = settings.concurrency.clamp(1, 4);
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

#[tauri::command]
pub async fn login(app: AppHandle, state: State<'_, AppState>) -> Result<SessionStatus, String> {
    let path = state.cli.path_or_err()?;
    let args = vec!["login".to_string()];
    let _ = cli::run(&app, &path, &args).await?;
    let status = cli::run(&app, &path, &vec!["status".to_string()]).await?;
    Ok(parse_status(&status.combined(), true))
}

#[tauri::command]
pub async fn logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let path = state.cli.path_or_err()?;
    cli::run(&app, &path, &vec!["logout".to_string()]).await?;
    Ok(())
}

async fn fetch_listing(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
    path: Option<&str>,
) -> Result<LsResponse, String> {
    let mut args = vec!["ls".to_string(), "--json".to_string()];
    if let Some(id) = folder_id {
        args.push(id.to_string());
    } else if let Some(path) = path {
        let path = normalize_path(path);
        if path != "/" {
            args.push(path);
        }
    }
    cli::run_json(app, cli_path, &args).await
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
) -> Result<LogicalListing, String> {
    let cli_path = state.cli.path_or_err()?;
    let settings = state.settings.read().unwrap().clone();

    // Virtual views always work on the root listing; nothing is moved in O2.
    let (base_folder, base_path) = if virtual_path.is_some() {
        (None, Some("/".to_string()))
    } else {
        (folder_id, path.clone())
    };
    let listing = fetch_listing(&app, &cli_path, base_folder, base_path.as_deref()).await?;
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

async fn create_folder_inner(
    app: &AppHandle,
    cli_path: &Path,
    parent_path: &str,
    name: &str,
) -> Result<RemoteFolder, String> {
    let parent_path = normalize_path(parent_path);
    let target = join_path(&parent_path, name);
    let args = vec!["mkdir".to_string(), target.clone()];
    if let Ok(out) = cli::run(app, cli_path, &args).await {
        if let Some(id) = transfer::parse_created_id(&out.combined()) {
            return Ok(RemoteFolder {
                id,
                name: name.to_string(),
                files: 0,
                size: 0,
            });
        }
    }

    // The folder may already exist (or the CLI did not print an id).
    let listing = fetch_listing(app, cli_path, None, Some(&parent_path)).await?;
    if let Some(folder) = listing.folders.into_iter().find(|f| f.name == name) {
        return Ok(folder);
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
    create_folder_inner(&app, &cli_path, &parent_path, &name).await
}

#[derive(Serialize)]
pub struct UploadSummary {
    pub uploaded: u64,
    pub failed: Vec<String>,
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
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
    let settings = state.settings.read().unwrap().clone();
    let chunk_size = settings.chunk_size_mb * 1024 * 1024;
    let mut uploaded = 0u64;
    let mut failed = Vec::new();

    for raw_path in paths {
        let path = PathBuf::from(&raw_path);
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                failed.push(format!("{raw_path}: {err}"));
                continue;
            }
        };

        if meta.is_file() {
            let transfer_id = uuid::Uuid::new_v4().to_string();
            match transfer::upload_one(
                &app,
                &cli_path,
                &path,
                folder_id,
                chunk_size,
                &transfer_id,
            )
            .await
            {
                Ok(_) => uploaded += 1,
                Err(err) => failed.push(format!("{raw_path}: {err}")),
            }
            continue;
        }

        if !meta.is_dir() {
            failed.push(format!("{raw_path}: no es un archivo ni una carpeta"));
            continue;
        }

        let root_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("carpeta")
            .to_string();
        let folder_transfer_id = uuid::Uuid::new_v4().to_string();
        let uploaded_before = uploaded;
        let _ = app.emit(
            "transfer-progress",
            TransferProgress {
                id: folder_transfer_id.clone(),
                kind: "upload".into(),
                name: root_name.clone(),
                phase: "scan".into(),
                current: 0,
                total: 0,
                bytes_done: 0,
                bytes_total: 0,
                message: "Preparando carpeta…".into(),
            },
        );
        let root_folder = match create_folder_inner(
            &app,
            &cli_path,
            &folder_path,
            &root_name,
        )
        .await
        {
            Ok(folder) => folder,
            Err(err) => {
                failed.push(format!("{raw_path}: {err}"));
                continue;
            }
        };

        let mut remote: HashMap<PathBuf, (Option<u64>, String)> = HashMap::new();
        remote.insert(path.clone(), (Some(root_folder.id), join_path(&folder_path, &root_name)));

        for entry in WalkDir::new(&path)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    failed.push(format!("{raw_path}: {err}"));
                    continue;
                }
            };
            let local = entry.path().to_path_buf();
            let Some(parent) = local.parent() else {
                continue;
            };
            let Some((parent_id, parent_remote)) = remote.get(parent).cloned() else {
                continue;
            };

            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                match create_folder_inner(
                    &app,
                    &cli_path,
                    &parent_remote,
                    &name,
                )
                .await
                {
                    Ok(folder) => {
                        remote.insert(local, (Some(folder.id), join_path(&parent_remote, &name)));
                    }
                    Err(err) => failed.push(format!("{}: {err}", local.display())),
                }
            } else if entry.file_type().is_file() {
                let transfer_id = uuid::Uuid::new_v4().to_string();
                match transfer::upload_one(
                    &app,
                    &cli_path,
                    &local,
                    parent_id,
                    chunk_size,
                    &transfer_id,
                )
                .await
                {
                    Ok(_) => uploaded += 1,
                    Err(err) => failed.push(format!("{}: {err}", local.display())),
                }
            }
        }

        let folder_uploaded = uploaded - uploaded_before;
        let _ = app.emit(
            "transfer-progress",
            TransferProgress {
                id: folder_transfer_id,
                kind: "upload".into(),
                name: root_name,
                phase: "done".into(),
                current: folder_uploaded,
                total: folder_uploaded,
                bytes_done: 0,
                bytes_total: 0,
                message: format!("Carpeta completada ({folder_uploaded} archivos)"),
            },
        );
    }

    Ok(UploadSummary { uploaded, failed })
}

#[tauri::command]
pub async fn download_entry(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    name: String,
    dest: String,
    chunked: bool,
    manifest_id: Option<u64>,
    folder_id: Option<u64>,
) -> Result<(), String> {
    let cli_path = state.cli.path_or_err()?;
    let dest = PathBuf::from(dest);
    let transfer_id = uuid::Uuid::new_v4().to_string();
    if chunked {
        let manifest_id = manifest_id.ok_or("Este archivo no tiene manifiesto")?;
        transfer::download_chunked(
            &app,
            &cli_path,
            folder_id,
            manifest_id,
            &dest,
            &transfer_id,
        )
        .await
    } else {
        transfer::download_normal(&app, &cli_path, id, &dest, &transfer_id, &name).await
    }
}

#[tauri::command]
pub async fn delete_entry(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    is_folder: bool,
) -> Result<(), String> {
    let cli_path = state.cli.path_or_err()?;
    let mut args = vec!["rm".to_string(), id.to_string()];
    if is_folder {
        args.push("-r".into());
        args.push("-y".into());
    }
    cli::run(&app, &cli_path, &args).await?;
    Ok(())
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
    let previous = state.cli.info();
    let info = cli::download_and_install(&app, &version, &url).await?;

    // Smoke test the new CLI against this account before making it current.
    // A bad release must not leave the app unable to list files. If there is
    // no session yet, or this is the first install, we cannot test the
    // listing; the next login will exercise it.
    let should_test = previous.available && previous.version != "0.0.0";
    if should_test {
        let test_args = vec!["ls".to_string(), "--json".to_string()];
        if let Err(err) =
            cli::run_json::<LsResponse>(&app, Path::new(&info.path), &test_args).await
        {
            let auth_problem = err.to_lowercase().contains("not logged")
                || err.to_lowercase().contains("login")
                || err.to_lowercase().contains("session");
            if !auth_problem {
                let _ = cli::write_current(&app, Path::new(&previous.path), &previous.version);
                return Err(format!(
                    "La versión {version} no ha pasado la prueba de listado; se mantiene la anterior. {err}"
                ));
            }
        }
    }

    state
        .cli
        .set(PathBuf::from(&info.path), info.version.clone(), info.source.clone());
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
}
