use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cli;
use crate::progress::{self, FileId};
use crate::types::{
    ChunkManifest, LogicalFile, ManifestPart, PartRef, RemoteFile, TransferProgress,
};

const READ_BUFFER: usize = 1024 * 1024;

fn emit(app: &AppHandle, progress: TransferProgress) {
    let _ = app.emit("transfer-progress", progress);
}

/// Mark a transfer as failed, so its row doesn't look stuck.
pub fn emit_failed(app: &AppHandle, transfer_id: &str, kind: &str, name: &str, err: &str) {
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: kind.into(),
            name: name.into(),
            phase: "error".into(),
            current: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            message: err.into(),
        },
    );
}

pub fn parse_created_id(raw: &str) -> Option<u64> {
    let marker = "(id:";
    let start = raw.find(marker)? + marker.len();
    let rest = &raw[start..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

fn append_folder_arg(args: &mut Vec<String>, folder_id: Option<u64>) {
    if let Some(id) = folder_id {
        args.push("--folder".into());
        args.push(id.to_string());
    }
}

/// A transfer whose data stops moving for this long is stopped and tried
/// again. The CLI sets no time limit on sending or receiving file data, so
/// a dead connection would otherwise hold up the queue forever.
const STALLED_AFTER: Duration = Duration::from_secs(3 * 60);

/// How long O2 may take while no data can move: before a download's first
/// byte (the CLI lists the whole account first) and once an upload is sent
/// (O2 stores it before answering).
const WAITING_LIMIT: Duration = Duration::from_secs(15 * 60);

/// The waits before each new attempt at a transfer that failed for a reason
/// that may pass. At least a minute, so that an upload O2 stored despite the
/// error is in its listings by then (that takes up to 60 s).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(4 * 60),
];

/// How the error of a transfer stopped by its `Watchdog` starts.
const STALLED: &str = "La transferencia se ha atascado";

/// Notices a transfer that has gone too long without moving.
struct Watchdog {
    last_move: Instant,
}

impl Watchdog {
    fn new() -> Self {
        Self {
            last_move: Instant::now(),
        }
    }

    fn moved(&mut self) {
        self.last_move = Instant::now();
    }

    /// Break once nothing has moved for `limit`.
    fn check(&self, limit: Duration) -> ControlFlow<String> {
        if self.last_move.elapsed() < limit {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(format!("{STALLED}: {} sin avanzar", minutes(limit)))
        }
    }
}

fn minutes(duration: Duration) -> String {
    format!("{} min", duration.as_secs().div_ceil(60))
}

/// Whether a failed transfer may work if tried again: the connection
/// failed, O2 was busy or the transfer stalled. Not when O2 refused it
/// (e.g. MED-1020, the 4 GB limit), the session expired or a local file
/// couldn't be read or written.
fn worth_retrying(err: &str) -> bool {
    err.starts_with(STALLED)
        || err.starts_with("HTTP request failed")
        || err.match_indices("(status ").any(|(at, marker)| {
            let code = err[at + marker.len()..].get(..3);
            matches!(code.and_then(|c| c.parse::<u16>().ok()), Some(429 | 500..=599))
        })
}

/// How long to wait before trying a failed transfer again, if at all.
fn retry_delay(err: &str, retries: usize) -> Option<Duration> {
    RETRY_DELAYS.get(retries).copied().filter(|_| worth_retrying(err))
}

/// What a transfer's row says while it waits to try again. The row shows
/// the first line; the error is there on hover.
fn retry_notice(err: &str, delay: Duration, retry: usize) -> String {
    format!(
        "Ha fallado; reintento {retry} de {} en {}…\n{err}",
        RETRY_DELAYS.len(),
        minutes(delay)
    )
}

fn gave_up(err: String, retries: usize) -> String {
    if retries == 0 {
        err
    } else {
        format!("Ha fallado {} veces: {err}", retries + 1)
    }
}

/// Upload `path` with the CLI and return its new ID, trying again when it
/// fails for a reason that may pass. `sent` is called with how much of the
/// file the CLI has read so far, each time that grows, and with 0 and a
/// notice while waiting to try again.
async fn cli_upload(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    mut sent: impl FnMut(u64, Option<&str>),
) -> Result<u64, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let size = std::fs::metadata(path)
        .map_err(|e| format!("No se puede leer {}: {e}", path.display()))?
        .len();
    let mut retries = 0;
    loop {
        let result: Result<u64, String> = async {
            // An upload can fail after O2 got all of it; sending it again
            // would make a duplicate.
            if retries > 0
                && let Some(id) = find_uploaded(app, cli_path, folder_id, &name, size).await?
            {
                return Ok(id);
            }
            upload_attempt(app, cli_path, path, folder_id, size, &mut sent).await
        }
        .await;
        let err = match result {
            Ok(id) => return Ok(id),
            Err(err) => err,
        };
        let Some(delay) = retry_delay(&err, retries) else {
            return Err(gave_up(err, retries));
        };
        retries += 1;
        sent(0, Some(&retry_notice(&err, delay, retries)));
        tokio::time::sleep(delay).await;
    }
}

/// The ID of a file named `name` with `size` bytes in `folder_id`, if O2
/// lists one.
async fn find_uploaded(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
    name: &str,
    size: u64,
) -> Result<Option<u64>, String> {
    let files = current_folder_files(app, cli_path, folder_id).await?;
    Ok(files
        .into_iter()
        .find(|file| file.name == name && file.size == Some(size))
        .map(|file| file.id))
}

/// One run of `o2cloud upload`, stopped if it stalls.
async fn upload_attempt(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    size: u64,
    sent: &mut impl FnMut(u64, Option<&str>),
) -> Result<u64, String> {
    let mut args = vec!["upload".to_string(), path.to_string_lossy().to_string()];
    append_folder_arg(&mut args, folder_id);
    let file = FileId::of(path);
    let mut last = 0;
    let mut open = false;
    let mut watchdog = Watchdog::new();
    let out = cli::run_watched(app, cli_path, &args, |pid| {
        // Where the CLI can't be seen reading the file, a stalled upload
        // looks like a slow one.
        let Some(file) = file.as_ref() else {
            return ControlFlow::Continue(());
        };
        let offset = progress::read_offset(pid, file);
        if offset.is_some() != open {
            open = offset.is_some();
            watchdog.moved();
        }
        if let Some(offset) = offset
            && offset > last
        {
            last = offset;
            watchdog.moved();
            sent(offset, None);
        }
        // Data moves while the CLI reads the file. Before, it is getting
        // ready; after, O2 is storing what it got.
        watchdog.check(if open && last < size {
            STALLED_AFTER
        } else {
            WAITING_LIMIT
        })
    })
    .await?;
    let out = out.combined();
    parse_created_id(&out).ok_or_else(|| {
        format!(
            "No se pudo leer el id de la subida de {}:\n{out}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })
}

/// Download media `id` to `path` with the CLI, trying again when it fails
/// for a reason that may pass. `received` is called with the size of the
/// file so far, each time that grows, and with 0 and a notice while waiting
/// to try again.
async fn cli_download(
    app: &AppHandle,
    cli_path: &Path,
    id: u64,
    path: &Path,
    mut received: impl FnMut(u64, Option<&str>),
) -> Result<(), String> {
    let mut retries = 0;
    loop {
        let err = match download_attempt(app, cli_path, id, path, &mut received).await {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };
        let Some(delay) = retry_delay(&err, retries) else {
            return Err(gave_up(err, retries));
        };
        retries += 1;
        received(0, Some(&retry_notice(&err, delay, retries)));
        tokio::time::sleep(delay).await;
    }
}

/// One run of `o2cloud download`, stopped if it stalls.
async fn download_attempt(
    app: &AppHandle,
    cli_path: &Path,
    id: u64,
    path: &Path,
    received: &mut impl FnMut(u64, Option<&str>),
) -> Result<(), String> {
    // `path` is always the app's own file. One left by a stopped attempt
    // would make the CLI refuse to write.
    let _ = tokio::fs::remove_file(path).await;
    let args = vec![
        "download".to_string(),
        id.to_string(),
        "-o".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let mut last = None;
    let mut watchdog = Watchdog::new();
    cli::run_watched(app, cli_path, &args, |_| {
        // The CLI creates the file once O2 starts sending it.
        if let Ok(meta) = std::fs::metadata(path)
            && Some(meta.len()) > last
        {
            last = Some(meta.len());
            watchdog.moved();
            received(meta.len(), None);
        }
        watchdog.check(if last.is_some() {
            STALLED_AFTER
        } else {
            WAITING_LIMIT
        })
    })
    .await
    .map(|_| ())
}

pub async fn current_folder_files(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
) -> Result<Vec<RemoteFile>, String> {
    let mut args = vec!["ls".to_string(), "--json".to_string()];
    if let Some(id) = folder_id {
        args.push(id.to_string());
    }
    let listing: crate::types::LsResponse = cli::run_json(app, cli_path, &args).await?;
    Ok(listing.files)
}

/// Files already in a folder: the sizes seen for each name.
pub type ExistingFiles = HashMap<String, Vec<u64>>;

pub fn existing_files(files: &[RemoteFile]) -> ExistingFiles {
    let mut existing = ExistingFiles::new();
    for file in files {
        if let Some(size) = file.size {
            existing.entry(file.name.clone()).or_default().push(size);
        }
    }
    existing
}

pub(crate) fn already_uploaded(existing: &ExistingFiles, name: &str, size: u64) -> bool {
    existing.get(name).is_some_and(|sizes| sizes.contains(&size))
}

/// Logical files already in a listing, with their full size. Unlike
/// `existing_files`, a completed chunked file appears under its logical name
/// (not its parts), which lets the Photos view skip it before exporting it.
pub fn logical_files_existing(files: &[RemoteFile]) -> ExistingFiles {
    let (logical, _) = group_logical_files(files.to_vec());
    let mut existing = ExistingFiles::new();
    for file in logical.into_iter().filter(|file| !file.incomplete) {
        existing.entry(file.name).or_default().push(file.size);
    }
    existing
}

pub enum UploadOutcome {
    Uploaded,
    /// A file with the same name and size was already in the folder, from
    /// an earlier upload. Uploading it again would only make a duplicate.
    Skipped,
}

/// A logical file is either a normal object or a group of chunks.
pub fn group_logical_files(files: Vec<RemoteFile>) -> (Vec<LogicalFile>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut manifests: HashMap<String, RemoteFile> = HashMap::new();
    let mut parts: HashMap<String, Vec<RemoteFile>> = HashMap::new();
    let mut normals: Vec<RemoteFile> = Vec::new();

    for file in files {
        if let Some(base) = file.name.strip_suffix(".o2manifest.json") {
            manifests.insert(base.to_string(), file);
        } else if let Some((base, _num)) = split_part_name(&file.name) {
            parts.entry(base).or_default().push(file);
        } else {
            normals.push(file);
        }
    }

    let mut logical = Vec::new();

    for file in normals {
        logical.push(LogicalFile {
            name: file.name,
            size: file.size.unwrap_or(0),
            date: file.date,
            chunked: false,
            manifest_id: None,
            normal_id: Some(file.id),
            parts: Vec::new(),
            incomplete: false,
        });
    }

    for (base, mut chunk_files) in parts {
        chunk_files.sort_by(|a, b| a.name.cmp(&b.name));
        let manifest = manifests.remove(&base);
        let total: u64 = chunk_files.iter().map(|f| f.size.unwrap_or(0)).sum();
        let refs: Vec<PartRef> = chunk_files
            .iter()
            .map(|f| PartRef {
                id: f.id,
                name: f.name.clone(),
                size: f.size.unwrap_or(0),
            })
            .collect();

        if manifest.is_none() {
            warnings.push(format!(
                "'{base}' tiene fragmentos pero no un manifiesto; se puede descargar, pero sin verificación."
            ));
        }

        logical.push(LogicalFile {
            name: base,
            size: total,
            date: chunk_files.first().and_then(|f| f.date.clone()),
            chunked: true,
            manifest_id: manifest.as_ref().map(|m| m.id),
            normal_id: None,
            parts: refs,
            incomplete: manifest.is_none(),
        });
    }

    for (base, manifest) in manifests {
        warnings.push(format!(
            "'{base}' tiene manifiesto pero no se han encontrado sus fragmentos."
        ));
        logical.push(LogicalFile {
            name: base,
            size: 0,
            date: manifest.date,
            chunked: true,
            manifest_id: Some(manifest.id),
            normal_id: None,
            parts: Vec::new(),
            incomplete: true,
        });
    }

    logical.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (logical, warnings)
}

fn split_part_name(name: &str) -> Option<(String, u64)> {
    let marker = ".o2part.";
    let idx = name.rfind(marker)?;
    let base = &name[..idx];
    let number = &name[idx + marker.len()..];
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((base.to_string(), number.parse().ok()?))
}

/// Upload one local file into `folder_id`, chunked if it is larger than
/// `chunk_size`. `existing` is what the folder already holds.
pub async fn upload_one(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    chunk_size: u64,
    transfer_id: &str,
    existing: &ExistingFiles,
) -> Result<UploadOutcome, String> {
    let result = upload_file(
        app,
        cli_path,
        path,
        folder_id,
        chunk_size,
        transfer_id,
        existing,
    )
    .await;
    if let Err(err) = &result {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        emit_failed(app, transfer_id, "upload", &name, err);
    }
    result
}

async fn upload_file(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    chunk_size: u64,
    transfer_id: &str,
    existing: &ExistingFiles,
) -> Result<UploadOutcome, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("No se puede leer {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} no es un archivo", path.display()));
    }
    let size = meta.len();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Nombre de archivo inválido")?
        .to_string();

    if size > chunk_size {
        return upload_chunked(
            app,
            cli_path,
            path,
            &name,
            size,
            folder_id,
            chunk_size,
            transfer_id,
            existing,
        )
        .await;
    }

    if already_uploaded(existing, &name, size) {
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name,
                phase: "done".into(),
                current: 1,
                total: 1,
                bytes_done: size,
                bytes_total: size,
                message: "Ya estaba en O2; no se vuelve a subir".into(),
            },
        );
        return Ok(UploadOutcome::Skipped);
    }

    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "upload".into(),
            name: name.clone(),
            phase: "upload".into(),
            current: 0,
            total: 1,
            bytes_done: 0,
            bytes_total: size,
            message: format!("Subiendo {}", name),
        },
    );
    let id = cli_upload(app, cli_path, path, folder_id, |sent, notice| {
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.clone(),
                phase: "upload".into(),
                current: 0,
                total: 1,
                bytes_done: sent.min(size),
                bytes_total: size,
                message: match notice {
                    Some(notice) => notice.to_string(),
                    // Once everything is sent, O2 still has to store it and answer.
                    None if sent >= size => "Enviado; O2 lo está procesando…".into(),
                    None => format!("Subiendo {name}"),
                },
            },
        );
    })
    .await?;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "upload".into(),
            name,
            phase: "done".into(),
            current: 1,
            total: 1,
            bytes_done: size,
            bytes_total: size,
            message: format!("Subido (id {id})"),
        },
    );
    Ok(UploadOutcome::Uploaded)
}

/// Split, hash and upload a large file one part at a time, so it only needs
/// one part's worth of free disk, then upload the manifest. Parts already in
/// the folder with the right name and size (from an interrupted upload) are
/// hashed but not uploaded again.
#[allow(clippy::too_many_arguments)]
async fn upload_chunked(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    name: &str,
    total_size: u64,
    folder_id: Option<u64>,
    chunk_size: u64,
    transfer_id: &str,
    existing: &ExistingFiles,
) -> Result<UploadOutcome, String> {
    let total_parts = total_size.div_ceil(chunk_size);
    let part_name_of = |index: u64| format!("{name}.o2part.{index:03}");
    let part_size_of = |index: u64| chunk_size.min(total_size - (index - 1) * chunk_size);
    let manifest_name = format!("{name}.o2manifest.json");

    if existing.contains_key(&manifest_name) {
        let same = (1..=total_parts)
            .all(|index| already_uploaded(existing, &part_name_of(index), part_size_of(index)));
        if !same {
            return Err(format!(
                "Ya hay un «{name}» troceado distinto en esta carpeta. Bórralo antes de volver a subirlo."
            ));
        }
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.to_string(),
                phase: "done".into(),
                current: total_parts,
                total: total_parts,
                bytes_done: total_size,
                bytes_total: total_size,
                message: "Ya estaba en O2; no se vuelve a subir".into(),
            },
        );
        return Ok(UploadOutcome::Skipped);
    }

    let temp_root = app_cache_dir(app)?.join("transfers").join(transfer_id);
    tokio::fs::create_dir_all(&temp_root)
        .await
        .map_err(|e| e.to_string())?;

    let result: Result<UploadOutcome, String> = async {
        let mut src = tokio::fs::File::open(path)
            .await
            .map_err(|e| format!("No se puede abrir {}: {e}", path.display()))?;
        let modified = src.metadata().await.ok().and_then(|m| m.modified().ok());
        let mut whole = Sha256::new();
        let mut manifest_parts = Vec::new();
        let mut buffer = vec![0u8; READ_BUFFER];
        let mut bytes_done = 0u64;
        let mut resumed = 0u64;

        for index in 1..=total_parts {
            let part_name = part_name_of(index);
            let size = part_size_of(index);
            let already_there = already_uploaded(existing, &part_name, size);
            let part_path = temp_root.join(&part_name);

            emit(
                app,
                TransferProgress {
                    id: transfer_id.into(),
                    kind: "upload".into(),
                    name: name.to_string(),
                    phase: "split".into(),
                    current: index - 1,
                    total: total_parts,
                    bytes_done,
                    bytes_total: total_size,
                    message: format!("Preparando fragmento {index}/{total_parts}…"),
                },
            );

            let mut dst = if already_there {
                None
            } else {
                Some(
                    tokio::fs::File::create(&part_path)
                        .await
                        .map_err(|e| e.to_string())?,
                )
            };
            let mut part_hasher = Sha256::new();
            let mut remaining = size;
            while remaining > 0 {
                let want = remaining.min(READ_BUFFER as u64) as usize;
                let read = src
                    .read(&mut buffer[..want])
                    .await
                    .map_err(|e| e.to_string())?;
                if read == 0 {
                    return Err(format!(
                        "{} ha cambiado de tamaño durante la subida",
                        path.display()
                    ));
                }
                if let Some(dst) = dst.as_mut() {
                    dst.write_all(&buffer[..read])
                        .await
                        .map_err(|e| e.to_string())?;
                }
                whole.update(&buffer[..read]);
                part_hasher.update(&buffer[..read]);
                remaining -= read as u64;
            }
            manifest_parts.push(ManifestPart {
                name: part_name.clone(),
                size,
                sha256: Some(hex::encode(part_hasher.finalize())),
            });

            if let Some(mut dst) = dst {
                dst.flush().await.map_err(|e| e.to_string())?;
                drop(dst);
                set_modified(&part_path, modified);
                emit(
                    app,
                    TransferProgress {
                        id: transfer_id.into(),
                        kind: "upload".into(),
                        name: name.to_string(),
                        phase: "upload".into(),
                        current: index - 1,
                        total: total_parts,
                        bytes_done,
                        bytes_total: total_size,
                        message: format!("Subiendo fragmento {index}/{total_parts}"),
                    },
                );
                cli_upload(app, cli_path, &part_path, folder_id, |sent, notice| {
                    emit(
                        app,
                        TransferProgress {
                            id: transfer_id.into(),
                            kind: "upload".into(),
                            name: name.to_string(),
                            phase: "upload".into(),
                            current: index - 1,
                            total: total_parts,
                            bytes_done: bytes_done + sent.min(size),
                            bytes_total: total_size,
                            message: match notice {
                                Some(notice) => format!("Fragmento {index}/{total_parts}: {notice}"),
                                None if sent >= size => format!(
                                    "Fragmento {index}/{total_parts} enviado; O2 lo está procesando…"
                                ),
                                None => format!("Subiendo fragmento {index}/{total_parts}"),
                            },
                        },
                    );
                })
                .await?;
                let _ = tokio::fs::remove_file(&part_path).await;
            } else {
                resumed += 1;
            }

            bytes_done += size;
            emit(
                app,
                TransferProgress {
                    id: transfer_id.into(),
                    kind: "upload".into(),
                    name: name.to_string(),
                    phase: "upload".into(),
                    current: index,
                    total: total_parts,
                    bytes_done,
                    bytes_total: total_size,
                    message: if already_there {
                        format!("Fragmento {index}/{total_parts} ya estaba subido")
                    } else {
                        format!("Fragmento {index}/{total_parts} subido")
                    },
                },
            );
        }

        let manifest = ChunkManifest {
            format: "o2cloud-chunks".into(),
            version: 1,
            name: name.to_string(),
            size: total_size,
            sha256: hex::encode(whole.finalize()),
            chunk_size,
            parts: manifest_parts,
            created_at: None,
        };
        let manifest_path = temp_root.join(&manifest_name);
        let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        tokio::fs::write(&manifest_path, manifest_json)
            .await
            .map_err(|e| e.to_string())?;
        set_modified(&manifest_path, modified);

        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.to_string(),
                phase: "manifest".into(),
                current: total_parts,
                total: total_parts,
                bytes_done: total_size,
                bytes_total: total_size,
                message: "Subiendo manifiesto…".into(),
            },
        );

        cli_upload(app, cli_path, &manifest_path, folder_id, |_, notice| {
            if let Some(notice) = notice {
                emit(
                    app,
                    TransferProgress {
                        id: transfer_id.into(),
                        kind: "upload".into(),
                        name: name.to_string(),
                        phase: "manifest".into(),
                        current: total_parts,
                        total: total_parts,
                        bytes_done: total_size,
                        bytes_total: total_size,
                        message: format!("Manifiesto: {notice}"),
                    },
                );
            }
        })
        .await?;

        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.to_string(),
                phase: "done".into(),
                current: total_parts,
                total: total_parts,
                bytes_done: total_size,
                bytes_total: total_size,
                message: if resumed > 0 {
                    format!("Subido en {total_parts} fragmentos ({resumed} ya estaban)")
                } else {
                    format!("Subido en {total_parts} fragmentos")
                },
            },
        );
        Ok(UploadOutcome::Uploaded)
    }
    .await;

    // Parts are deleted as they go up; this also removes what an error left.
    let _ = tokio::fs::remove_dir_all(&temp_root).await;
    result
}

/// Give a part or manifest the original's modification time. The CLI sends
/// a file's modification time to O2 as its date, so without this a chunked
/// file is dated when it was uploaded rather than like the original.
fn set_modified(path: &Path, time: Option<std::time::SystemTime>) {
    let Some(time) = time else { return };
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(time);
    }
}

/// Where a download is written until it is complete and verified: next to
/// the destination, so putting it in place is a rename.
fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".o2download");
    dest.with_file_name(name)
}

/// Put a finished download in place. The save dialog has already asked
/// before replacing an existing file.
async fn move_into_place(partial: &Path, dest: &Path) -> Result<(), String> {
    // On Windows a rename does not replace an existing file.
    #[cfg(windows)]
    {
        if dest.exists() {
            tokio::fs::remove_file(dest)
                .await
                .map_err(|e| format!("No se puede reemplazar {}: {e}", dest.display()))?;
        }
    }
    tokio::fs::rename(partial, dest)
        .await
        .map_err(|e| format!("No se puede guardar {}: {e}", dest.display()))
}

fn part_id(candidates: &[PartRef], part: &ManifestPart) -> Option<u64> {
    candidates
        .iter()
        .find(|c| c.name == part.name && c.size == part.size)
        .map(|c| c.id)
}

/// Download a chunked file: fetch the manifest, then each part, appending
/// it to the output and checking its SHA-256 on the way, then the SHA-256
/// of the whole file. `known_parts` are the parts in the listing on screen.
#[allow(clippy::too_many_arguments)]
pub async fn download_chunked(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
    manifest_id: u64,
    known_parts: &[PartRef],
    name: &str,
    dest: &Path,
    transfer_id: &str,
) -> Result<(), String> {
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: name.to_string(),
            phase: "download".into(),
            current: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: known_parts.iter().map(|p| p.size).sum(),
            message: "Preparando descarga…".into(),
        },
    );
    let temp_root = app_cache_dir(app)?.join("transfers").join(transfer_id);
    tokio::fs::create_dir_all(&temp_root)
        .await
        .map_err(|e| e.to_string())?;
    let partial = partial_path(dest);
    // Left over by an earlier attempt that was killed.
    let _ = tokio::fs::remove_file(&partial).await;

    let result: Result<ChunkManifest, String> = async {
        let manifest_path = temp_root.join("manifest.json");
        cli_download(app, cli_path, manifest_id, &manifest_path, |_, notice| {
            if let Some(notice) = notice {
                emit(
                    app,
                    TransferProgress {
                        id: transfer_id.into(),
                        kind: "download".into(),
                        name: name.to_string(),
                        phase: "download".into(),
                        current: 0,
                        total: 0,
                        bytes_done: 0,
                        bytes_total: known_parts.iter().map(|p| p.size).sum(),
                        message: format!("Manifiesto: {notice}"),
                    },
                );
            }
        })
        .await?;
        let raw = tokio::fs::read(&manifest_path)
            .await
            .map_err(|e| e.to_string())?;
        let manifest: ChunkManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("Manifiesto inválido: {e}"))?;

        // Only list the folder again if the listing on screen lacks a part.
        let mut candidates = known_parts.to_vec();
        if manifest
            .parts
            .iter()
            .any(|part| part_id(&candidates, part).is_none())
        {
            candidates = current_folder_files(app, cli_path, folder_id)
                .await?
                .into_iter()
                .map(|f| PartRef {
                    id: f.id,
                    name: f.name,
                    size: f.size.unwrap_or(0),
                })
                .collect();
        }

        let mut out = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| format!("No se puede crear {}: {e}", partial.display()))?;
        let total_parts = manifest.parts.len() as u64;
        let mut whole = Sha256::new();
        let mut buffer = vec![0u8; READ_BUFFER];
        let mut bytes_done = 0u64;

        for (idx, part) in manifest.parts.iter().enumerate() {
            let id = part_id(&candidates, part)
                .ok_or_else(|| format!("Falta el fragmento {}", part.name))?;
            let part_path = temp_root.join(&part.name);

            // The CLI looks the part up in the account before data flows.
            emit(
                app,
                TransferProgress {
                    id: transfer_id.into(),
                    kind: "download".into(),
                    name: name.to_string(),
                    phase: "download".into(),
                    current: idx as u64,
                    total: total_parts,
                    bytes_done,
                    bytes_total: manifest.size,
                    message: format!("Preparando fragmento {}/{}…", idx + 1, total_parts),
                },
            );
            cli_download(app, cli_path, id, &part_path, |received, notice| {
                emit(
                    app,
                    TransferProgress {
                        id: transfer_id.into(),
                        kind: "download".into(),
                        name: name.to_string(),
                        phase: "download".into(),
                        current: idx as u64,
                        total: total_parts,
                        bytes_done: bytes_done + received.min(part.size),
                        bytes_total: manifest.size,
                        message: match notice {
                            Some(notice) => format!("Fragmento {}/{}: {notice}", idx + 1, total_parts),
                            None => format!("Descargando fragmento {}/{}", idx + 1, total_parts),
                        },
                    },
                );
            })
            .await?;

            let mut part_file = tokio::fs::File::open(&part_path)
                .await
                .map_err(|e| e.to_string())?;
            let mut part_hasher = Sha256::new();
            let mut part_len = 0u64;
            loop {
                let read = part_file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                out.write_all(&buffer[..read])
                    .await
                    .map_err(|e| e.to_string())?;
                part_hasher.update(&buffer[..read]);
                whole.update(&buffer[..read]);
                part_len += read as u64;
            }
            drop(part_file);
            let _ = tokio::fs::remove_file(&part_path).await;

            if part_len != part.size {
                return Err(format!(
                    "Tamaño inesperado en {}: {} (esperado {})",
                    part.name, part_len, part.size
                ));
            }
            if let Some(expected) = &part.sha256
                && &hex::encode(part_hasher.finalize()) != expected
            {
                return Err(format!("SHA-256 incorrecto en {}", part.name));
            }
            bytes_done += part.size;
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);

        let actual = hex::encode(whole.finalize());
        if actual != manifest.sha256 {
            return Err(format!(
                "El SHA-256 del archivo reconstruido no coincide (esperado {}, obtenido {}); no se ha guardado.",
                manifest.sha256, actual
            ));
        }
        move_into_place(&partial, dest).await?;
        Ok(manifest)
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&temp_root).await;
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(err) => {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(err);
        }
    };

    let total_parts = manifest.parts.len() as u64;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: name.to_string(),
            phase: "done".into(),
            current: total_parts,
            total: total_parts,
            bytes_done: manifest.size,
            bytes_total: manifest.size,
            message: "Descarga y verificación completadas".into(),
        },
    );
    Ok(())
}

/// Download a normal file. `size` is its size in the listing, if known.
pub async fn download_normal(
    app: &AppHandle,
    cli_path: &Path,
    id: u64,
    size: Option<u64>,
    dest: &Path,
    transfer_id: &str,
    name: &str,
) -> Result<(), String> {
    let size = size.unwrap_or(0);
    // The CLI looks the file up in the account before data flows.
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: name.to_string(),
            phase: "download".into(),
            current: 0,
            total: 1,
            bytes_done: 0,
            bytes_total: size,
            message: "Preparando descarga…".into(),
        },
    );
    let partial = partial_path(dest);
    // Left over by an earlier attempt that was killed.
    let _ = tokio::fs::remove_file(&partial).await;
    let downloaded = cli_download(app, cli_path, id, &partial, |received, notice| {
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "download".into(),
                name: name.to_string(),
                phase: "download".into(),
                current: 0,
                total: 1,
                bytes_done: if size > 0 { received.min(size) } else { received },
                bytes_total: size,
                message: match notice {
                    Some(notice) => notice.to_string(),
                    None => format!("Descargando {name}"),
                },
            },
        );
    })
    .await;
    let result = match downloaded {
        Ok(()) => move_into_place(&partial, dest).await,
        Err(err) => Err(err),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result?;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: name.to_string(),
            phase: "done".into(),
            current: 1,
            total: 1,
            bytes_done: size,
            bytes_total: size,
            message: "Descarga completada".into(),
        },
    );
    Ok(())
}

pub fn app_cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("o2cloud");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u64, name: &str, size: u64) -> RemoteFile {
        RemoteFile {
            id,
            name: name.to_string(),
            size: Some(size),
            date: None,
            path: None,
        }
    }

    #[test]
    fn logical_file_keys_cover_completed_chunks_but_not_incomplete_ones() {
        let existing = logical_files_existing(&[
            file(1, "movie.mkv.o2part.001", 100),
            file(2, "movie.mkv.o2part.002", 50),
            file(3, "movie.mkv.o2manifest.json", 200),
            file(4, "broken.mkv.o2part.001", 10),
            file(5, "notes.txt", 10),
        ]);
        assert_eq!(existing.get("movie.mkv"), Some(&vec![150]));
        assert_eq!(existing.get("notes.txt"), Some(&vec![10]));
        assert!(!existing.contains_key("broken.mkv"));
    }

    #[test]
    fn groups_chunks_and_manifest_into_one_logical_file() {
        let files = vec![
            file(1, "movie.mkv.o2part.001", 100),
            file(2, "movie.mkv.o2part.002", 50),
            file(3, "movie.mkv.o2manifest.json", 200),
            file(4, "notes.txt", 10),
        ];
        let (logical, warnings) = group_logical_files(files);
        assert!(warnings.is_empty());
        assert_eq!(logical.len(), 2);

        let movie = logical.iter().find(|f| f.name == "movie.mkv").unwrap();
        assert!(movie.chunked);
        assert_eq!(movie.size, 150);
        assert_eq!(movie.manifest_id, Some(3));
        assert_eq!(movie.parts.len(), 2);
        assert!(!movie.incomplete);

        let notes = logical.iter().find(|f| f.name == "notes.txt").unwrap();
        assert!(!notes.chunked);
        assert_eq!(notes.normal_id, Some(4));
    }

    #[test]
    fn marks_chunks_without_manifest_as_incomplete() {
        let files = vec![file(1, "movie.mkv.o2part.001", 100)];
        let (logical, warnings) = group_logical_files(files);
        assert_eq!(warnings.len(), 1);
        assert_eq!(logical.len(), 1);
        assert!(logical[0].chunked);
        assert!(logical[0].incomplete);
        assert_eq!(logical[0].manifest_id, None);
    }

    #[test]
    fn parses_ids_from_cli_output() {
        assert_eq!(
            parse_created_id("✓ Created folder /x/ (id:35704586)"),
            Some(35704586)
        );
        assert_eq!(
            parse_created_id("/x/ already exists (id:35704586)"),
            Some(35704586)
        );
        assert_eq!(
            parse_created_id("✓ Uploaded successfully (id: 1544124548)"),
            Some(1544124548)
        );
        assert_eq!(parse_created_id("no id here"), None);
    }

    #[test]
    fn recognises_files_already_uploaded_by_name_and_size() {
        let existing = existing_files(&[
            file(1, "a.jpg", 10),
            file(2, "a.jpg", 20),
            RemoteFile {
                id: 3,
                name: "unknown-size.jpg".into(),
                size: None,
                date: None,
                path: None,
            },
        ]);
        assert!(already_uploaded(&existing, "a.jpg", 10));
        assert!(already_uploaded(&existing, "a.jpg", 20));
        assert!(!already_uploaded(&existing, "a.jpg", 30));
        assert!(!already_uploaded(&existing, "b.jpg", 10));
        assert!(!already_uploaded(&existing, "unknown-size.jpg", 0));
    }

    #[test]
    fn finds_parts_by_name_and_size() {
        let candidates = vec![
            PartRef {
                id: 1,
                name: "m.o2part.001".into(),
                size: 99,
            },
            PartRef {
                id: 2,
                name: "m.o2part.001".into(),
                size: 100,
            },
        ];
        let part = ManifestPart {
            name: "m.o2part.001".into(),
            size: 100,
            sha256: None,
        };
        assert_eq!(part_id(&candidates, &part), Some(2));
    }

    #[test]
    fn parts_take_the_original_modification_time() {
        let path = std::env::temp_dir().join(format!("o2-mtime-{}", std::process::id()));
        std::fs::write(&path, b"part").unwrap();
        let original = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_662_150_659);
        set_modified(&path, Some(original));
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn downloads_go_to_a_partial_file_next_to_the_destination() {
        assert_eq!(
            partial_path(Path::new("/tmp/x/movie.mkv")),
            PathBuf::from("/tmp/x/movie.mkv.o2download")
        );
    }

    #[test]
    fn retries_network_failures_busy_servers_and_stalls_only() {
        assert!(worth_retrying(
            "HTTP request failed: error sending request for url (https://upload.cloud.o2online.es/sapi/upload)"
        ));
        assert!(worth_retrying("API error: Upload failed (status 502 Bad Gateway): <html>"));
        assert!(worth_retrying("API error: Download failed (status 429 Too Many Requests): "));
        assert!(worth_retrying(&format!("{STALLED}: 3 min sin avanzar")));

        assert!(!worth_retrying(
            "API error: Upload rejected: MED-1020: File size exceeds configured max file size limit"
        ));
        assert!(!worth_retrying("API error: Download failed (status 404 Not Found): "));
        assert!(!worth_retrying("Authentication failed: session expired"));
        assert!(!worth_retrying("I/O error: Cannot write to /a: No space left on device"));
        assert!(!worth_retrying("API error: Upload failed (status 5"));
    }

    #[test]
    fn gives_up_after_the_last_retry() {
        let err = "HTTP request failed: connection reset";
        let delays: Vec<_> = (0..4).map(|retries| retry_delay(err, retries)).collect();
        assert_eq!(
            delays,
            [
                Some(Duration::from_secs(60)),
                Some(Duration::from_secs(120)),
                Some(Duration::from_secs(240)),
                None
            ]
        );
        assert_eq!(retry_delay("Not found: media 7", 0), None);
        assert_eq!(gave_up(err.into(), 0), err);
        assert_eq!(gave_up(err.into(), 3), format!("Ha fallado 4 veces: {err}"));
    }

    #[test]
    fn retry_notice_shows_the_wait_first_and_the_error_below() {
        let notice = retry_notice("HTTP request failed: timed out", Duration::from_secs(120), 2);
        assert_eq!(
            notice,
            "Ha fallado; reintento 2 de 3 en 2 min…\nHTTP request failed: timed out"
        );
    }

    #[test]
    fn watchdog_breaks_only_after_nothing_moved_for_the_limit() {
        let mut watchdog = Watchdog::new();
        assert_eq!(watchdog.check(Duration::from_secs(60)), ControlFlow::Continue(()));
        watchdog.last_move -= Duration::from_secs(61);
        let ControlFlow::Break(reason) = watchdog.check(Duration::from_secs(60)) else {
            panic!("a minute without moving should stop it");
        };
        assert!(worth_retrying(&reason));
        watchdog.moved();
        assert_eq!(watchdog.check(Duration::from_secs(60)), ControlFlow::Continue(()));
    }
}
