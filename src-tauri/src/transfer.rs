use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cli;
use crate::types::{
    ChunkManifest, LogicalFile, ManifestPart, PartRef, RemoteFile, TransferProgress,
};

const READ_BUFFER: usize = 1024 * 1024;
const PROGRESS_STEP: u64 = 32 * 1024 * 1024;

fn emit(app: &AppHandle, progress: TransferProgress) {
    let _ = app.emit("transfer-progress", progress);
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

/// A logical file is either a normal object or a group of chunks.
pub fn group_logical_files(files: Vec<RemoteFile>) -> (Vec<LogicalFile>, Vec<String>) {
    use std::collections::HashMap;

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

pub async fn upload_one(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    chunk_size: u64,
    transfer_id: &str,
) -> Result<String, String> {
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
        upload_chunked(app, cli_path, path, folder_id, chunk_size, transfer_id).await
    } else {
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
        let mut args = vec!["upload".to_string(), path.to_string_lossy().to_string()];
        append_folder_arg(&mut args, folder_id);
        let out = cli::run(app, cli_path, &args).await?;
        let out = out.combined();
        let id = parse_created_id(&out)
            .ok_or_else(|| format!("No se pudo leer el id de la subida:\n{out}"))?;
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
        Ok(id.to_string())
    }
}

pub async fn upload_chunked(
    app: &AppHandle,
    cli_path: &Path,
    path: &Path,
    folder_id: Option<u64>,
    chunk_size: u64,
    transfer_id: &str,
) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Nombre de archivo inválido")?
        .to_string();
    let total_size = tokio::fs::metadata(path)
        .await
        .map_err(|e| e.to_string())?
        .len();
    let total_parts = total_size.div_ceil(chunk_size);

    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "upload".into(),
            name: name.clone(),
            phase: "split".into(),
            current: 0,
            total: total_parts,
            bytes_done: 0,
            bytes_total: total_size,
            message: "Calculando SHA-256 y preparando fragmentos…".into(),
        },
    );

    let temp_root = app_cache_dir(app)?.join("transfers").join(transfer_id);
    tokio::fs::create_dir_all(&temp_root)
        .await
        .map_err(|e| e.to_string())?;

    let (manifest, part_paths) = build_parts(app, path, &temp_root, chunk_size, transfer_id).await?;

    for (idx, part_path) in part_paths.iter().enumerate() {
        let part_name = part_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("part")
            .to_string();
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.clone(),
                phase: "upload".into(),
                current: idx as u64,
                total: total_parts,
                bytes_done: idx as u64 * chunk_size,
                bytes_total: total_size,
                message: format!("Subiendo fragmento {}/{}", idx + 1, total_parts),
            },
        );
        let mut args = vec![
            "upload".to_string(),
            part_path.to_string_lossy().to_string(),
        ];
        append_folder_arg(&mut args, folder_id);
        let out = cli::run(app, cli_path, &args).await?;
        let out = out.combined();
        let _id = parse_created_id(&out)
            .ok_or_else(|| format!("No se pudo leer el id del fragmento {part_name}:\n{out}"))?;
        let _ = tokio::fs::remove_file(part_path).await;
        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "upload".into(),
                name: name.clone(),
                phase: "upload".into(),
                current: (idx + 1) as u64,
                total: total_parts,
                bytes_done: ((idx + 1) as u64 * chunk_size).min(total_size),
                bytes_total: total_size,
                message: format!("Fragmento {}/{} subido", idx + 1, total_parts),
            },
        );
    }

    let manifest_path = temp_root.join(format!("{name}.o2manifest.json"));
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    tokio::fs::write(&manifest_path, manifest_json)
        .await
        .map_err(|e| e.to_string())?;

    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "upload".into(),
            name: name.clone(),
            phase: "manifest".into(),
            current: total_parts,
            total: total_parts,
            bytes_done: total_size,
            bytes_total: total_size,
            message: "Subiendo manifiesto…".into(),
        },
    );

    let mut args = vec![
        "upload".to_string(),
        manifest_path.to_string_lossy().to_string(),
    ];
    append_folder_arg(&mut args, folder_id);
    let out = cli::run(app, cli_path, &args).await?;
    let out = out.combined();
    let manifest_id = parse_created_id(&out)
        .ok_or_else(|| format!("No se pudo leer el id del manifiesto:\n{out}"))?;

    let _ = tokio::fs::remove_dir_all(&temp_root).await;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "upload".into(),
            name: name.clone(),
            phase: "done".into(),
            current: total_parts,
            total: total_parts,
            bytes_done: total_size,
            bytes_total: total_size,
            message: format!("Subido en {total_parts} fragmentos"),
        },
    );
    Ok(manifest_id.to_string())
}

async fn build_parts(
    app: &AppHandle,
    path: &Path,
    temp_root: &Path,
    chunk_size: u64,
    transfer_id: &str,
) -> Result<(ChunkManifest, Vec<PathBuf>), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Nombre de archivo inválido")?
        .to_string();
    let mut src = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("No se puede abrir {}: {e}", path.display()))?;
    let total_size = src.metadata().await.map_err(|e| e.to_string())?.len();
    let total_parts = total_size.div_ceil(chunk_size);

    let mut whole = Sha256::new();
    let mut manifest_parts = Vec::new();
    let mut part_paths = Vec::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    let mut bytes_done = 0u64;
    let mut last_emit = 0u64;

    for index in 1..=total_parts {
        let part_name = format!("{name}.o2part.{index:03}");
        let part_path = temp_root.join(&part_name);
        let mut dst = tokio::fs::File::create(&part_path)
            .await
            .map_err(|e| e.to_string())?;
        let mut part_hasher = Sha256::new();
        let mut remaining = chunk_size;
        let mut part_size = 0u64;

        while remaining > 0 {
            let want = remaining.min(READ_BUFFER as u64) as usize;
            let read = src.read(&mut buffer[..want]).await.map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            dst.write_all(&buffer[..read]).await.map_err(|e| e.to_string())?;
            whole.update(&buffer[..read]);
            part_hasher.update(&buffer[..read]);
            remaining -= read as u64;
            part_size += read as u64;
            bytes_done += read as u64;

            if bytes_done - last_emit >= PROGRESS_STEP {
                last_emit = bytes_done;
                emit(
                    app,
                    TransferProgress {
                        id: transfer_id.into(),
                        kind: "upload".into(),
                        name: name.clone(),
                        phase: "split".into(),
                        current: index - 1,
                        total: total_parts,
                        bytes_done,
                        bytes_total: total_size,
                        message: "Calculando SHA-256 y preparando fragmentos…".into(),
                    },
                );
            }
        }
        dst.flush().await.map_err(|e| e.to_string())?;
        drop(dst);

        manifest_parts.push(ManifestPart {
            name: part_name,
            size: part_size,
            sha256: Some(hex::encode(part_hasher.finalize())),
        });
        part_paths.push(part_path);
    }

    let manifest = ChunkManifest {
        format: "o2cloud-chunks".into(),
        version: 1,
        name,
        size: total_size,
        sha256: hex::encode(whole.finalize()),
        chunk_size,
        parts: manifest_parts,
        created_at: None,
    };
    Ok((manifest, part_paths))
}

pub async fn download_chunked(
    app: &AppHandle,
    cli_path: &Path,
    folder_id: Option<u64>,
    manifest_id: u64,
    dest: &Path,
    transfer_id: &str,
) -> Result<(), String> {
    if dest.exists() {
        return Err(format!("{} ya existe", dest.display()));
    }
    let temp_root = app_cache_dir(app)?.join("transfers").join(transfer_id);
    tokio::fs::create_dir_all(&temp_root)
        .await
        .map_err(|e| e.to_string())?;

    let manifest_path = temp_root.join("manifest.json");
    let args = vec![
        "download".to_string(),
        manifest_id.to_string(),
        "-o".to_string(),
        manifest_path.to_string_lossy().to_string(),
    ];
    cli::run(app, cli_path, &args).await?;
    let raw = tokio::fs::read(&manifest_path)
        .await
        .map_err(|e| e.to_string())?;
    let manifest: ChunkManifest = serde_json::from_slice(&raw)
        .map_err(|e| format!("Manifiesto inválido: {e}"))?;

    let remote_files = current_folder_files(app, cli_path, folder_id).await?;
    use std::collections::HashMap;
    let mut by_name: HashMap<String, u64> = HashMap::new();
    for file in remote_files {
        by_name.insert(file.name, file.id);
    }

    let mut out = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("No se puede crear {}: {e}", dest.display()))?;
    let total_parts = manifest.parts.len() as u64;

    for (idx, part) in manifest.parts.iter().enumerate() {
        let id = by_name
            .get(&part.name)
            .ok_or_else(|| format!("Falta el fragmento {}", part.name))?;
        let part_path = temp_root.join(&part.name);

        emit(
            app,
            TransferProgress {
                id: transfer_id.into(),
                kind: "download".into(),
                name: manifest.name.clone(),
                phase: "download".into(),
                current: idx as u64,
                total: total_parts,
                bytes_done: part.size * idx as u64,
                bytes_total: manifest.size,
                message: format!("Descargando fragmento {}/{}", idx + 1, total_parts),
            },
        );

        let args = vec![
            "download".to_string(),
            id.to_string(),
            "-o".to_string(),
            part_path.to_string_lossy().to_string(),
        ];
        cli::run(app, cli_path, &args).await?;

        let meta = tokio::fs::metadata(&part_path)
            .await
            .map_err(|e| e.to_string())?;
        if meta.len() != part.size {
            return Err(format!(
                "Tamaño inesperado en {}: {} (esperado {})",
                part.name,
                meta.len(),
                part.size
            ));
        }

        if let Some(expected) = &part.sha256 {
            let actual = sha256_file(&part_path).await?;
            if &actual != expected {
                return Err(format!("SHA-256 incorrecto en {}", part.name));
            }
        }

        let mut part_file = tokio::fs::File::open(&part_path)
            .await
            .map_err(|e| e.to_string())?;
        tokio::io::copy(&mut part_file, &mut out)
            .await
            .map_err(|e| e.to_string())?;
        drop(part_file);
        let _ = tokio::fs::remove_file(&part_path).await;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);

    let actual = sha256_file(dest).await?;
    if actual != manifest.sha256 {
        return Err(format!(
            "El SHA-256 del archivo reconstruido no coincide (esperado {}, obtenido {}). Se conserva el archivo para que puedas revisarlo.",
            manifest.sha256, actual
        ));
    }

    let _ = tokio::fs::remove_dir_all(&temp_root).await;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: manifest.name.clone(),
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

pub async fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("No se puede abrir {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub async fn download_normal(
    app: &AppHandle,
    cli_path: &Path,
    id: u64,
    dest: &Path,
    transfer_id: &str,
    name: &str,
) -> Result<(), String> {
    if dest.exists() {
        return Err(format!("{} ya existe", dest.display()));
    }
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
            bytes_total: 0,
            message: format!("Descargando {name}"),
        },
    );
    let args = vec![
        "download".to_string(),
        id.to_string(),
        "-o".to_string(),
        dest.to_string_lossy().to_string(),
    ];
    cli::run(app, cli_path, &args).await?;
    emit(
        app,
        TransferProgress {
            id: transfer_id.into(),
            kind: "download".into(),
            name: name.to_string(),
            phase: "done".into(),
            current: 1,
            total: 1,
            bytes_done: 0,
            bytes_total: 0,
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
            parse_created_id("✓ Uploaded successfully (id: 1544124548)"),
            Some(1544124548)
        );
        assert_eq!(parse_created_id("no id here"), None);
    }
}
