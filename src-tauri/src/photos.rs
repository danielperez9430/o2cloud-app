//! macOS Photos library access (PhotoKit) for uploading originals straight
//! from Fotos. The actual PhotoKit calls live in `photos.m`; this module
//! turns them into async Tauri commands and progress events.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Emitter;

#[cfg(target_os = "macos")]
use crate::types::TransferProgress;

/// An item in the Photos library, as listed by PhotoKit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoAsset {
    /// PhotoKit's `localIdentifier`, used to fetch the asset again.
    pub id: String,
    /// Original filename, such as `IMG_1234.MOV`.
    pub name: String,
    /// Original size in bytes, or 0 when PhotoKit does not expose it.
    #[serde(default)]
    pub size: u64,
    /// Creation date in Unix milliseconds.
    #[serde(default)]
    pub date: Option<i64>,
    /// Video duration in seconds; 0 for photos.
    #[serde(default)]
    pub duration: f64,
    /// "video" or "photo".
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoList {
    pub assets: Vec<PhotoAsset>,
    /// Matching assets read; when `truncated`, there was at least one more.
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoAlbum {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoAlbums {
    #[serde(default)]
    pub albums: Vec<PhotoAlbum>,
}

pub fn available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(target_os = "macos")]
fn status_name(status: i32) -> &'static str {
    match status {
        0 => "not_determined",
        1 => "restricted",
        2 => "denied",
        3 => "authorized",
        4 => "limited",
        _ => "unknown",
    }
}

pub fn authorization_status() -> String {
    #[cfg(target_os = "macos")]
    {
        let status = unsafe { ffi::o2_photos_authorization_status() };
        status_name(status).to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unavailable".to_string()
    }
}

pub async fn request_access() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let status = tokio::task::spawn_blocking(|| unsafe { ffi::o2_photos_request_access() })
            .await
            .map_err(|err| err.to_string())?;
        Ok(status_name(status).to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Subir desde Fotos solo está disponible en macOS.".into())
    }
}

pub async fn list(
    media_type: &str,
    min_size: u64,
    album_id: Option<String>,
    favorites_only: bool,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    offset: u32,
    limit: u32,
) -> Result<PhotoList, String> {
    #[cfg(target_os = "macos")]
    {
        // PHAssetMediaType: 1 = image, 2 = video, anything else = all.
        let media_type = match media_type {
            "photo" => 1,
            "video" => 2,
            _ => 0,
        };
        let album = album_id
            .filter(|id| !id.is_empty())
            .map(|id| std::ffi::CString::new(id).map_err(|_| "Álbum de Fotos inválido.".to_string()))
            .transpose()?;
        let raw = tokio::task::spawn_blocking(move || unsafe {
            take_string(ffi::o2_photos_list_json(
                media_type,
                min_size,
                limit,
                offset,
                album.as_ref().map_or(std::ptr::null(), |id| id.as_ptr()),
                if favorites_only { 1 } else { 0 },
                start_ms.unwrap_or(0),
                end_ms.unwrap_or(0),
            ))
        })
        .await
        .map_err(|err| err.to_string())??;
        parse_payload(&raw)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (
            media_type,
            min_size,
            album_id,
            favorites_only,
            start_ms,
            end_ms,
            offset,
            limit,
        );
        Err("Subir desde Fotos solo está disponible en macOS.".into())
    }
}

pub async fn albums() -> Result<PhotoAlbums, String> {
    #[cfg(target_os = "macos")]
    {
        let raw = tokio::task::spawn_blocking(|| unsafe {
            take_string(ffi::o2_photos_albums_json())
        })
        .await
        .map_err(|err| err.to_string())??;
        parse_payload(&raw)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Subir desde Fotos solo está disponible en macOS.".into())
    }
}

pub async fn asset(identifier: &str) -> Result<PhotoAsset, String> {
    #[cfg(target_os = "macos")]
    {
        let identifier = std::ffi::CString::new(identifier)
            .map_err(|_| "Identificador de Fotos inválido.".to_string())?;
        let raw = tokio::task::spawn_blocking(move || unsafe {
            take_string(ffi::o2_photos_asset_json(identifier.as_ptr()))
        })
        .await
        .map_err(|err| err.to_string())??;
        parse_payload(&raw)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        Err("Subir desde Fotos solo está disponible en macOS.".into())
    }
}

/// A filename that is safe to use inside the app's cache directory.
pub fn safe_file_name(name: &str) -> String {
    let base = name
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .replace(['/', '\\'], "_");
    if base.is_empty() || base == "." || base == ".." {
        return "archivo".into();
    }
    // Most filesystems stop at 255 bytes; leave room for a suffix. Count
    // bytes, not chars: a 200-char emoji name would still be too long.
    let mut safe = String::new();
    for character in base.chars() {
        if safe.len() + character.len_utf8() > 200 {
            break;
        }
        safe.push(character);
    }
    safe
}

/// Export the original file of `asset` to `destination`, reporting progress
/// as an upload in the transfers popover. Blocking PhotoKit work runs on a
/// worker thread.
pub async fn export(
    app: AppHandle,
    transfer_id: String,
    asset: PhotoAsset,
    destination: PathBuf,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let identifier = std::ffi::CString::new(asset.id.clone())
            .map_err(|_| "Identificador de Fotos inválido.".to_string())?;
        let destination = std::ffi::CString::new(destination.to_string_lossy().as_bytes())
            .map_err(|_| "Ruta temporal inválida.".to_string())?;
        let context = ExportContext {
            app,
            transfer_id,
            name: asset.name,
            total: asset.size,
            last_emit: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(1),
            ),
        };

        tokio::task::spawn_blocking(move || {
            let mut context = context;
            let error = unsafe {
                ffi::o2_photos_export(
                    identifier.as_ptr(),
                    destination.as_ptr(),
                    &mut context as *mut ExportContext as *mut std::os::raw::c_void,
                    Some(export_progress),
                )
            };
            if error.is_null() {
                Ok(())
            } else {
                let message = unsafe { std::ffi::CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { ffi::o2_photos_free(error) };
                Err(message)
            }
        })
        .await
        .map_err(|err| err.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, transfer_id, asset, destination);
        Err("Subir desde Fotos solo está disponible en macOS.".into())
    }
}

#[cfg(target_os = "macos")]
struct ExportContext {
    app: AppHandle,
    transfer_id: String,
    name: String,
    total: u64,
    /// PhotoKit calls the data handler very often; the UI only needs a few
    /// updates per second.
    last_emit: std::sync::Mutex<std::time::Instant>,
}

#[cfg(target_os = "macos")]
extern "C" fn export_progress(context: *mut std::os::raw::c_void, written: u64, total: u64) {
    if context.is_null() {
        return;
    }
    let context = unsafe { &*(context as *const ExportContext) };
    let total = if total > 0 { total } else { context.total };

    if total == 0 || written < total {
        let now = std::time::Instant::now();
        let Ok(mut last_emit) = context.last_emit.lock() else {
            return;
        };
        if now.duration_since(*last_emit) < std::time::Duration::from_millis(250) {
            return;
        }
        *last_emit = now;
    }

    let message = if total > 0 && written >= total {
        "Descargado de Fotos; preparando la subida…"
    } else {
        "Descargando de Fotos…"
    };
    let _ = context.app.emit(
        "transfer-progress",
        TransferProgress {
            id: context.transfer_id.clone(),
            kind: "upload".into(),
            name: context.name.clone(),
            phase: "export".into(),
            current: 0,
            total: 1,
            bytes_done: written,
            bytes_total: total,
            message: message.to_string(),
        },
    );
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    pub type ProgressCallback = extern "C" fn(*mut c_void, u64, u64);

    unsafe extern "C" {
        pub fn o2_photos_authorization_status() -> c_int;
        pub fn o2_photos_request_access() -> c_int;
        pub fn o2_photos_albums_json() -> *mut c_char;
        pub fn o2_photos_list_json(
            media_type: c_int,
            min_size: u64,
            limit: u32,
            offset: u32,
            album_id: *const c_char,
            favorites_only: c_int,
            start_ms: i64,
            end_ms: i64,
        ) -> *mut c_char;
        pub fn o2_photos_asset_json(identifier: *const c_char) -> *mut c_char;
        pub fn o2_photos_export(
            identifier: *const c_char,
            destination: *const c_char,
            context: *mut c_void,
            progress: Option<ProgressCallback>,
        ) -> *mut c_char;
        pub fn o2_photos_free(value: *mut c_char);
    }
}

#[cfg(target_os = "macos")]
fn take_string(pointer: *mut std::os::raw::c_char) -> Result<String, String> {
    if pointer.is_null() {
        return Err("Fotos no ha devuelto datos.".into());
    }
    let text = unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::o2_photos_free(pointer) };
    if text.is_empty() {
        return Err("Fotos ha devuelto una respuesta vacía.".into());
    }
    Ok(text)
}

#[cfg(target_os = "macos")]
fn parse_payload<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|err| format!("Respuesta inválida de Fotos: {err}"))?;
    if let Some(error) = value.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_string());
    }
    serde_json::from_value(value).map_err(|err| format!("Respuesta inválida de Fotos: {err}"))
}

#[cfg(test)]
mod tests {
    use super::safe_file_name;
    #[cfg(target_os = "macos")]
    use super::{PhotoList, parse_payload};

    #[cfg(target_os = "macos")]
    #[test]
    fn photo_payloads_are_parsed_and_errors_are_surfaced() {
        let list: PhotoList = parse_payload(
            r#"{"assets":[{"id":"L0/1","name":"IMG_0001.MOV","size":433430,"date":1700000000000,"duration":2.5,"media_type":"video","width":1920,"height":1080}],"total":1,"truncated":false}"#,
        )
        .unwrap();
        assert_eq!(list.total, 1);
        assert!(!list.truncated);
        assert_eq!(list.assets[0].name, "IMG_0001.MOV");
        assert_eq!(list.assets[0].size, 433_430);
        assert_eq!(list.assets[0].date, Some(1_700_000_000_000));

        let list: PhotoList =
            parse_payload(r#"{"assets":[{"id":"L0/2","name":"a.jpg","date":null}]}"#).unwrap();
        assert_eq!(list.assets[0].date, None);

        let error: Result<PhotoList, String> = parse_payload(r#"{"error":"Sin acceso"}"#);
        assert_eq!(error.unwrap_err(), "Sin acceso");
    }

    #[test]
    fn photo_names_stay_inside_the_cache_directory() {
        assert_eq!(safe_file_name("IMG_1234.MOV"), "IMG_1234.MOV");
        assert_eq!(safe_file_name("../IMG_1234.MOV"), "IMG_1234.MOV");
        assert_eq!(safe_file_name("a/b\\c.mov"), "c.mov");
        assert_eq!(safe_file_name("  "), "archivo");
        assert_eq!(safe_file_name(".."), "archivo");
    }

    #[test]
    fn long_photo_names_are_cut_to_a_safe_length() {
        let name = "x".repeat(400);
        assert_eq!(safe_file_name(&name).chars().count(), 200);
        let name = "é".repeat(300);
        assert!(safe_file_name(&name).len() <= 200);
    }
}
