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

/// A file as the app presents it: either a normal object or a logical file
/// made of chunks (`<name>.o2part.001`, ...) plus a manifest.
#[derive(Debug, Clone, Serialize)]
pub struct LogicalFile {
    pub name: String,
    pub size: u64,
    pub date: Option<String>,
    pub chunked: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Chunk size in MiB. Kept below O2's 4 GB per-file limit.
    #[serde(default = "default_chunk_mb")]
    pub chunk_size_mb: u64,
    /// Show a virtual "Cámara" folder at the root and hide its media from the
    /// root listing. Purely a UI view; nothing is moved in O2.
    #[serde(default = "default_true")]
    pub group_camera: bool,
}

fn default_chunk_mb() -> u64 {
    3500
}
fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            chunk_size_mb: default_chunk_mb(),
            group_camera: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CliInfo {
    pub path: String,
    pub version: String,
    pub source: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub logged_in: bool,
    pub raw: String,
    pub phone: Option<String>,
    pub storage: Option<String>,
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
