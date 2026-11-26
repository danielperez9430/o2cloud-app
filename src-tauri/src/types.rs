use serde::{Deserialize, Serialize};

/// A folder as returned by `o2cloud ls --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFolder {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub size: u64,
}

/// A media item as returned by `o2cloud ls --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFile {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
    /// `o2cloud ls --json` renders dates as ISO-8601 strings (or null).
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// Raw JSON shape produced by `o2cloud ls --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsResponse {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub folders: Vec<RemoteFolder>,
    #[serde(default)]
    pub files: Vec<RemoteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartRef {
    pub id: u64,
    pub name: String,
    pub size: u64,
}

/// A file as the app presents it: a normal object, an app chunk group
/// (`<name>.o2part.001`, ... plus `.o2manifest.json`), or a CLI split group
/// (`<name>.001`, ... plus `.o2split.json`).
#[derive(Debug, Clone, Serialize)]
pub struct LogicalFile {
    pub name: String,
    pub size: u64,
    pub date: Option<String>,
    pub chunked: bool,
    /// True for a CLI-native split, downloaded by asking the CLI to join it.
    pub cli_split: bool,
    pub manifest_id: Option<u64>,
    pub normal_id: Option<u64>,
    pub parts: Vec<PartRef>,
    /// True when chunks exist but the manifest is missing.
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VirtualFolder {
    pub key: String,
    pub name: String,
    pub files: u64,
    pub size: u64,
    /// "camera" or "year"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogicalListing {
    pub path: String,
    pub folders: Vec<RemoteFolder>,
    pub files: Vec<LogicalFile>,
    pub warnings: Vec<String>,
    pub virtual_folders: Vec<VirtualFolder>,
    /// True when this is the last listing saved on disk; refresh in the
    /// background after showing it.
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub chunk_size: u64,
    pub parts: Vec<ManifestPart>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPart {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Manifest for a file split by the CLI (`name.001`, `name.002`, …). The
/// app writes it in a later stage so a CLI split can still be verified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSplitManifest {
    pub format: String,
    #[serde(default)]
    pub version: u32,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub part_size: Option<u64>,
    #[serde(default)]
    pub parts: Vec<ManifestPart>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Show a virtual "Cámara" folder at the root and hide its media from the
    /// root listing. Purely a UI view; nothing is moved in O2.
    #[serde(default = "default_true")]
    pub group_camera: bool,
    /// Hash non-media files during preflight so renamed duplicates already in
    /// the shared library are found before uploading. Off by default because
    /// it reads every selected file once.
    #[serde(default)]
    pub hash_non_media: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            group_camera: true,
            hash_non_media: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CliInfo {
    pub path: String,
    pub version: String,
    pub source: String,
    pub available: bool,
    /// False when the CLI is older than the version the app needs.
    pub supported: bool,
    pub minimum_version: String,
}

/// Storage figures shared by `status --json` and `quota --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageSpace {
    #[serde(default)]
    pub quota: u64,
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub free: u64,
    /// Bytes in the trash.
    #[serde(default)]
    pub softdeleted: u64,
    /// The plan has no storage limit.
    #[serde(default)]
    pub nolimit: bool,
}

/// Raw shape produced by `o2cloud status --json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusOutput {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub renews_silently: bool,
    #[serde(default)]
    pub storage: Option<StorageSpace>,
}

/// Raw shape produced by `o2cloud mkdir --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct MkdirOutput {
    pub id: u64,
    #[serde(default)]
    pub created: bool,
}

/// Raw shape produced by `o2cloud upload --json`. From 0.9.0 on, a file the
/// CLI splits (over its 4 GB limit) has `parts` instead of `id`; the app
/// chunks first, so it always expects one `id`, but parse both shapes so a
/// future CLI cannot turn this into a confusing "missing field" error.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UploadOutput {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub parts: Vec<u64>,
    /// What the CLI did: `uploaded`, `updated`, `skipped`, `differs`…
    #[serde(default)]
    pub action: Option<String>,
    /// The SHA-256 of the file, when the CLI sent all of it in this run.
    #[serde(default)]
    pub sha256: Option<String>,
}

impl UploadOutput {
    /// The single media ID, or a clear error when the CLI split the upload.
    pub fn single_id(&self) -> Result<u64, String> {
        if let Some(id) = self.id {
            return Ok(id);
        }
        if !self.parts.is_empty() {
            return Err(format!(
                "El CLI ha dividido el archivo en {} partes por superar su límite de 4 GB. \
                 La app trocea los archivos antes de subirlos; actualiza la app.",
                self.parts.len()
            ));
        }
        Err("El CLI no devolvió el ID del archivo subido.".into())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub logged_in: bool,
    pub raw: String,
    pub phone: Option<String>,
    pub since: Option<String>,
    pub session: Option<String>,
    pub renews_silently: bool,
    pub storage: Option<StorageSpace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliRelease {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferProgress {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub phase: String,
    pub current: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub message: String,
}
