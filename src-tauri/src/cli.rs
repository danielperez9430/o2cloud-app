use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::types::{CliInfo, CliRelease};

const RELEASES_API: &str = "https://api.github.com/repos/danielperez9430/o2cloud-cli/releases/latest";
const USER_AGENT: &str = "o2cloud-desktop/0.1";

pub struct CliManager {
    path: RwLock<PathBuf>,
    version: RwLock<String>,
    source: RwLock<String>,
}

impl CliManager {
    pub fn discover(app: &AppHandle) -> Self {
        match discover_cli(app) {
            Ok((path, version, source)) => Self {
                path: RwLock::new(path),
                version: RwLock::new(version),
                source: RwLock::new(source),
            },
            Err(_) => Self {
                path: RwLock::new(PathBuf::new()),
                version: RwLock::new("0.0.0".into()),
                source: RwLock::new("none".into()),
            },
        }
    }

    pub fn is_available(&self) -> bool {
        !self.path.read().unwrap().as_os_str().is_empty()
    }

    pub fn path_or_err(&self) -> Result<PathBuf, String> {
        let path = self.path();
        if path.as_os_str().is_empty() {
            Err("El CLI de O2 Cloud no está instalado. Instálalo desde Ajustes.".into())
        } else {
            Ok(path)
        }
    }

    pub fn info(&self) -> CliInfo {
        CliInfo {
            path: self.path.read().unwrap().to_string_lossy().to_string(),
            version: self.version.read().unwrap().clone(),
            source: self.source.read().unwrap().clone(),
            available: self.is_available(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.path.read().unwrap().clone()
    }

    pub fn set(&self, path: PathBuf, version: String, source: String) {
        *self.path.write().unwrap() = path;
        *self.version.write().unwrap() = version;
        *self.source.write().unwrap() = source;
    }
}

pub fn exe_name() -> &'static str {
    if cfg!(windows) {
        "o2cloud.exe"
    } else {
        "o2cloud"
    }
}

fn bundled_dir() -> String {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-aarch64".into()
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x86_64".into()
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64".into()
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64".into()
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64".into()
    } else {
        "unknown".into()
    }
}

fn managed_current_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("cli");
    let file = dir.join("current.json");
    let raw = std::fs::read_to_string(file).ok()?;
    #[derive(Deserialize)]
    struct Current {
        path: String,
    }
    let current: Current = serde_json::from_str(&raw).ok()?;
    let path = PathBuf::from(current.path);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn version_of(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

fn parse_version(raw: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("o2cloud "))
        .map(|v| v.trim().to_string())
}

fn discover_cli(app: &AppHandle) -> Result<(PathBuf, String, String), String> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();

    if let Ok(path) = std::env::var("O2CLOUD_CLI") {
        let path = PathBuf::from(path);
        candidates.push((path, "env".into()));
    }

    if let Some(path) = managed_current_path(app) {
        candidates.push((path, "managed".into()));
    }

    if let Ok(resource_dir) = app.path().resource_dir() {
        let bundled = resource_dir
            .join("bin")
            .join(bundled_dir())
            .join(exe_name());
        candidates.push((bundled, "bundled".into()));
    }

    if let Ok(path) = which::which("o2cloud") {
        candidates.push((path, "system".into()));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push((home.join(".local/bin").join(exe_name()), "system".into()));
        candidates.push((home.join(".cargo/bin").join(exe_name()), "system".into()));
    }

    for (path, source) in candidates {
        if !path.is_file() {
            continue;
        }
        if let Some(version) = version_of(&path) {
            return Ok((path, version, source));
        }
    }

    Err("No se ha encontrado el CLI de O2 Cloud. Instálalo con `cargo install o2cloud-cli` o descarga un binario desde GitHub.".into())
}

#[derive(Serialize, Clone)]
struct LogLine {
    stream: String,
    line: String,
}

pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    /// Some CLI commands (upload, mkdir, delete) print their human-readable
    /// result to stderr. Use whichever stream carries the payload.
    pub fn combined(&self) -> String {
        let mut out = self.stdout.clone();
        if !self.stderr.trim().is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        out
    }

    pub fn text(&self) -> &str {
        if self.stdout.trim().is_empty() {
            &self.stderr
        } else {
            &self.stdout
        }
    }
}

async fn collect_stream<R: AsyncRead + Unpin>(
    reader: R,
    app: AppHandle,
    stream: &'static str,
) -> String {
    let mut lines = BufReader::new(reader).lines();
    let mut collected = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        collected.push_str(&line);
        collected.push('\n');
        let _ = app.emit(
            "cli-log",
            LogLine {
                stream: stream.to_string(),
                line,
            },
        );
    }
    collected
}

/// Run the CLI and capture its output. Stdout/stderr lines are forwarded to
/// the frontend as `cli-log` events.
pub async fn run(app: &AppHandle, path: &Path, args: &[String]) -> Result<CliOutput, String> {
    let mut cmd = tokio::process::Command::new(path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("No se pudo ejecutar {}: {e}", path.display()))?;
    let stdout = child.stdout.take().ok_or("sin stdout")?;
    let stderr = child.stderr.take().ok_or("sin stderr")?;

    let out_task = tokio::spawn(collect_stream(stdout, app.clone(), "stdout"));
    let err_task = tokio::spawn(collect_stream(stderr, app.clone(), "stderr"));

    let status = child.wait().await.map_err(|e| e.to_string())?;
    let stdout = out_task.await.map_err(|e| e.to_string())?;
    let stderr = err_task.await.map_err(|e| e.to_string())?;

    if !status.success() {
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(detail.trim().to_string());
    }
    Ok(CliOutput { stdout, stderr })
}

pub async fn run_json<T: for<'de> Deserialize<'de>>(
    app: &AppHandle,
    path: &Path,
    args: &[String],
) -> Result<T, String> {
    let output = run(app, path, args).await?;
    let raw = output.text();
    let json_start = raw.find('{').ok_or_else(|| {
        format!(
            "El CLI no devolvió JSON: {}",
            raw.chars().take(300).collect::<String>()
        )
    })?;
    serde_json::from_str(&raw[json_start..])
        .map_err(|e| format!("JSON inválido del CLI: {e}\n{}", raw.chars().take(500).collect::<String>()))
}

fn asset_name_for_platform() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("o2cloud-aarch64-apple-darwin.tar.gz")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("o2cloud-x86_64-apple-darwin.tar.gz")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("o2cloud-x86_64-pc-windows-msvc.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("o2cloud-x86_64-unknown-linux-gnu.tar.gz")
    } else {
        None
    }
}

fn clean_version(tag: &str) -> String {
    tag.trim_start_matches('v').to_string()
}

pub async fn check_update(current: &str) -> Result<CliRelease, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(RELEASES_API)
        .send()
        .await
        .map_err(|e| format!("No se pudo consultar GitHub: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub respondió HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let tag = json["tag_name"].as_str().unwrap_or_default();
    let latest = clean_version(tag);
    let asset_name = asset_name_for_platform();
    let asset = asset_name.and_then(|name| {
        json["assets"]
            .as_array()?
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .cloned()
    });
    let asset_url = asset
        .as_ref()
        .and_then(|a| a["browser_download_url"].as_str())
        .map(|s| s.to_string());
    let notes = json["body"].as_str().map(|s| s.to_string());

    let update_available = match (
        semver::Version::parse(current),
        semver::Version::parse(&latest),
    ) {
        (Ok(a), Ok(b)) => b > a,
        _ => latest != current,
    };

    Ok(CliRelease {
        current: current.to_string(),
        latest: if latest.is_empty() { None } else { Some(latest) },
        update_available,
        asset_name: asset_name.map(|s| s.to_string()),
        asset_url,
        notes,
    })
}

pub async fn download_and_install(
    app: &AppHandle,
    version: &str,
    url: &str,
) -> Result<CliInfo, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Descarga fallida: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Descarga fallida: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;

    let cli_root = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("cli");
    let target_dir = cli_root.join("versions").join(version);
    std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;

    let temp_dir = cli_root.join("tmp");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

    let archive_name = url.rsplit('/').next().unwrap_or("download");
    let archive_path = temp_dir.join(archive_name);
    std::fs::write(&archive_path, &bytes).map_err(|e| e.to_string())?;

    let extracted = if archive_name.ends_with(".zip") {
        extract_zip(&archive_path, &temp_dir)?
    } else {
        extract_tar_gz(&archive_path, &temp_dir)?
    };

    let target = target_dir.join(exe_name());
    std::fs::copy(&extracted, &target).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(target_os = "macos")]
    {
        // A binary downloaded by the app normally has no quarantine attribute,
        // but clear it defensively if a previous copy did.
        let _ = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&target)
            .output();
    }

    let installed_version = version_of(&target)
        .ok_or_else(|| "El binario descargado no responde a --version".to_string())?;
    if installed_version != version {
        return Err(format!(
            "La versión instalada ({installed_version}) no coincide con la esperada ({version})"
        ));
    }

    write_current(app, &target, &installed_version)?;

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(CliInfo {
        path: target.to_string_lossy().to_string(),
        version: installed_version,
        source: "managed".into(),
        available: true,
    })
}

#[derive(Serialize, Deserialize)]
struct CliCurrent {
    version: String,
    path: String,
}

pub fn write_current(app: &AppHandle, path: &Path, version: &str) -> Result<(), String> {
    let cli_root = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("cli");
    std::fs::create_dir_all(&cli_root).map_err(|e| e.to_string())?;
    let current = CliCurrent {
        version: version.to_string(),
        path: path.to_string_lossy().to_string(),
    };
    std::fs::write(
        cli_root.join("current.json"),
        serde_json::to_vec_pretty(&current).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(dest).map_err(|e| format!("Error al extraer tar.gz: {e}"))?;
    find_binary(dest).ok_or_else(|| "El archivo no contiene el binario o2cloud".into())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("ZIP inválido: {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let out = dest.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
    }
    find_binary(dest).ok_or_else(|| "El archivo no contiene el binario o2cloud".into())
}

fn find_binary(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(exe_name());
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_binary(&path) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(exe_name()) {
            return Some(path);
        }
    }
    None
}
