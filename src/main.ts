import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { join } from "@tauri-apps/api/path";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";

interface RemoteFolder {
  id: number;
  name: string;
  files: number;
  size: number;
}

interface CreatedFolder {
  folder: RemoteFolder;
  created: boolean;
}

interface PartRef {
  id: number;
  name: string;
  size: number;
}

interface LogicalFile {
  name: string;
  size: number;
  date: string | number | null;
  chunked: boolean;
  manifest_id: number | null;
  normal_id: number | null;
  parts: PartRef[];
  incomplete: boolean;
}

interface VirtualFolder {
  key: string;
  name: string;
  files: number;
  size: number;
  kind: "camera" | "year" | "all" | string;
}

interface LogicalListing {
  path: string;
  folders: RemoteFolder[];
  files: LogicalFile[];
  warnings: string[];
  virtual_folders: VirtualFolder[];
}

interface CliInfo {
  path: string;
  version: string;
  source: string;
  available: boolean;
}

interface SessionStatus {
  logged_in: boolean;
  raw: string;
  phone: string | null;
  storage: string | null;
}

interface Settings {
  chunk_size_mb: number;
  group_camera: boolean;
}

interface CliRelease {
  current: string;
  latest: string | null;
  update_available: boolean;
  asset_name: string | null;
  asset_url: string | null;
  notes: string | null;
}

interface TransferProgress {
  id: string;
  kind: "upload" | "download" | string;
  name: string;
  phase: string;
  current: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
  message: string;
}

interface UploadSummary {
  uploaded: number;
  skipped: number;
  failed: string[];
  library_warning?: string | null;
}

interface LibraryStats {
  files: number;
  bytes: number;
  copies: number;
  devices: number;
  last_sync: number | null;
  pending_snapshot: boolean;
  device_id: string;
}

interface LibraryCopy {
  folder_id: number | null;
  folder_path: string;
  remote_id: number | null;
  remote_name: string;
  display_name: string;
}

interface PhotoLibraryMatch {
  asset_id: string;
  sha256: string | null;
  copies: LibraryCopy[];
  in_target: boolean;
}

interface PathCheck {
  path: string;
  sha256: string | null;
  copies: LibraryCopy[];
  in_target: boolean;
  is_file: boolean;
}

interface PhotoAsset {
  id: string;
  name: string;
  size: number;
  date: number | null;
  duration: number;
  media_type: "video" | "photo" | string;
  width: number;
  height: number;
}

interface PhotoList {
  assets: PhotoAsset[];
  total: number;
  truncated: boolean;
}

interface PhotosAccess {
  available: boolean;
  status: string;
}

interface Crumb {
  id: number | null;
  name: string;
  path: string;
  virtual?: string;
}

type SortKey = "name" | "size" | "date";

/** A row of the file list. */
type VirtualEntry = { kind: "virtual"; key: string; name: string; folder: VirtualFolder };
type FolderEntry = { kind: "folder"; key: string; name: string; folder: RemoteFolder };
type FileEntry = { kind: "file"; key: string; name: string; file: LogicalFile };
type Entry = VirtualEntry | FolderEntry | FileEntry;

type MenuEntry =
  | { label: string; hint?: string; danger?: boolean; disabled?: boolean; run: () => void }
  | "separator";

const ROOT: Crumb = { id: null, name: "O2 Cloud", path: "/" };
const CAMERA: Crumb = { id: null, name: "Cámara", path: "/", virtual: "camera" };
const PAGE = 500;
const PHOTO_PAGE = 300;
/** O2's per-file limit; the Photos view can filter to the files that hit it. */
const LARGE_FILE = 4 * 1024 ** 3;
const SORT_STORAGE = "o2cloud.sort";
/** Folders created in this session that are known to be empty, with expiry. */
const knownEmptyFolders = new Map<number, number>();
const EMPTY_FOLDER_GRACE_MS = 2 * 60_000;

const state = {
  stack: [ROOT] as Crumb[],
  listing: null as LogicalListing | null,
  /** Folders at the root, listed in the sidebar. */
  rootFolders: [] as RemoteFolder[],
  cli: null as CliInfo | null,
  session: null as SessionStatus | null,
  settings: { chunk_size_mb: 3500, group_camera: true } as Settings,
  queue: new Map<string, TransferProgress>(),
  pendingRelease: null as CliRelease | null,
  loading: false,
  visibleLimit: PAGE,
  uploadsInFlight: 0,
  sort: loadSort(),
  filter: "",
  selected: new Set<string>(),
  /** Where a Shift range starts, and the row the arrow keys move from. */
  anchor: null as string | null,
  cursor: null as string | null,
  /** Rows being moved to the trash. */
  trashing: new Set<string>(),
  libraryStats: null as LibraryStats | null,
  photos: {
    available: false,
    status: "unknown",
    assets: [] as PhotoAsset[],
    total: 0,
    truncated: false,
    selected: new Set<string>(),
    matches: new Map<string, PhotoLibraryMatch>(),
    loading: false,
    syncingLibrary: false,
    /** Resolves when the current library match finishes; uploads wait for it. */
    matchPromise: null as Promise<void> | null,
    uploading: false,
    filter: "",
    mediaType: "video",
    onlyLarge: false,
    hideExisting: false,
    visibleLimit: PHOTO_PAGE,
  },
};

const $ = <T extends HTMLElement>(selector: string): T => {
  const el = document.querySelector<T>(selector);
  if (!el) throw new Error(`No existe ${selector}`);
  return el;
};

function currentCrumb(): Crumb {
  return state.stack[state.stack.length - 1];
}

function crumbKey(crumb: Crumb): string {
  return `${crumb.id ?? ""}|${crumb.path}|${crumb.virtual ?? ""}`;
}

function joinPath(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent.replace(/\/+$/, "")}/${name}`;
}

function plural(count: number, one: string, many: string): string {
  return `${count.toLocaleString("es-ES")} ${count === 1 ? one : many}`;
}

function formatSize(bytes: number | null | undefined): string {
  if (bytes == null || bytes < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = value >= 100 || unit === 0 ? 0 : 1;
  return `${value.toFixed(digits).replace(".", ",")} ${units[unit]}`;
}

function timeOf(value: string | number | null): number | null {
  if (value == null || value === "") return null;
  const time = new Date(value).getTime();
  return Number.isNaN(time) ? null : time;
}

function formatDate(value: string | number | null): string {
  const time = timeOf(value);
  if (time == null) return "—";
  return new Date(time).toLocaleString("es-ES", {
    day: "2-digit",
    month: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatAgo(timestamp: number | null): string {
  if (timestamp == null) return "nunca";
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return "hace un momento";
  if (seconds < 3600) return `hace ${Math.round(seconds / 60)} min`;
  if (seconds < 86400) return `hace ${Math.round(seconds / 3600)} h`;
  return `hace ${Math.round(seconds / 86400)} días`;
}

function toast(
  message: string,
  type: "info" | "success" | "error" = "info",
  timeout = 5000,
): () => void {
  const container = $("#toast-container");
  const el = document.createElement("div");
  el.className = `toast ${type}`;
  el.textContent = message;
  container.appendChild(el);
  let leaving = false;
  const dismiss = () => {
    if (leaving) return;
    leaving = true;
    el.classList.add("leaving");
    window.setTimeout(() => el.remove(), 250);
  };
  window.setTimeout(dismiss, timeout);
  return dismiss;
}

function isSessionError(message: string): boolean {
  return /sesión de O2 ha caducado|Authentication failed|Not logged in/i.test(message);
}

/** Show an error; if it says the O2 session is gone, ask for a new login. */
function reportError(error: unknown, timeout = 10000) {
  const message = String(error);
  toast(message, "error", timeout);
  if (isSessionError(message)) void refreshSession();
}

function loadSort(): { key: SortKey; dir: 1 | -1 } {
  try {
    const saved = JSON.parse(localStorage.getItem(SORT_STORAGE) ?? "null");
    if (
      saved &&
      ["name", "size", "date"].includes(saved.key) &&
      (saved.dir === 1 || saved.dir === -1)
    ) {
      return saved;
    }
  } catch {
    // Storage unavailable: use the default.
  }
  return { key: "name", dir: 1 };
}

function saveSort() {
  try {
    localStorage.setItem(SORT_STORAGE, JSON.stringify(state.sort));
  } catch {
    // Not remembered this time.
  }
}

// ---------- Modals ----------

function setLoginStatus(message: string, type: "info" | "error" = "info") {
  const el = $("#login-status");
  el.textContent = message;
  el.className = `login-status ${type === "error" ? "error" : ""}`;
}

function showLoginModal() {
  $("#login-modal").classList.remove("hidden");
  setLoginStatus("");
}

function hideLoginModal() {
  $("#login-modal").classList.add("hidden");
}

function showSettings() {
  populateSettings();
  $("#settings-modal").classList.remove("hidden");
}

function hideSettings() {
  $("#settings-modal").classList.add("hidden");
}

function anyModalOpen(): boolean {
  return [...document.querySelectorAll(".modal")].some((m) => !m.classList.contains("hidden"));
}

// ---------- Top bar ----------

function renderSession() {
  const badge = $("#session-badge");
  const text = $("#session-text");
  if (state.session?.logged_in) {
    badge.classList.add("online");
    badge.classList.remove("offline");
    text.textContent = "Conectado";
    badge.title = [state.session.phone, state.session.storage].filter(Boolean).join(" · ");
  } else {
    badge.classList.add("offline");
    badge.classList.remove("online");
    text.textContent = "Sin sesión";
    badge.title = "Pulsa para iniciar sesión";
  }
}

const SIZE_UNITS = ["B", "KB", "MB", "GB", "TB"];

/** "547.0 GB" as printed by the CLI (powers of 1024), in bytes. */
function parseCliSize(text: string | undefined): number | null {
  const match = text?.match(/([\d.]+)\s*(B|KB|MB|GB|TB)\b/i);
  if (!match) return null;
  return Number(match[1]) * 1024 ** SIZE_UNITS.indexOf(match[2].toUpperCase());
}

async function refreshQuota() {
  const text = $("#quota-text");
  const bar = $("#quota-bar");
  try {
    // "Used:   547.0 GB of 10.0 TB (5.3%)", "Free:   9.5 TB", "Trash:  17.8 GB"
    const raw = await invoke<string>("get_quota");
    const line = (label: string) => raw.match(new RegExp(`${label}:\\s*([^\\n]+)`, "i"))?.[1];
    const [usedText, totalText] = (line("Used") ?? "").split(/\s+of\s+/i);
    const used = parseCliSize(usedText);
    const total = parseCliSize(totalText);
    const free = parseCliSize(line("Free"));
    const trash = parseCliSize(line("Trash"));
    if (used == null) {
      text.textContent = raw.split("\n")[0] || "—";
      bar.classList.add("hidden");
      return;
    }
    text.textContent = total ? `${formatSize(used)} de ${formatSize(total)}` : `${formatSize(used)} usados`;
    bar.classList.toggle("hidden", !total);
    if (total) $("#quota-fill").style.width = `${Math.min(100, (used / total) * 100)}%`;
    $("#quota").title = [
      free != null ? `Libre: ${formatSize(free)}` : "",
      trash ? `En la papelera: ${formatSize(trash)}` : "",
    ]
      .filter(Boolean)
      .join(" · ");
  } catch {
    text.textContent = "—";
    bar.classList.add("hidden");
  }
}

// ---------- Sidebar and path bar ----------

function renderPlaces() {
  const nav = $("#places");
  nav.replaceChildren();
  const top = state.stack[1];
  const places: { icon: string; label: string; stack: Crumb[]; active: boolean }[] = [
    { icon: "☁", label: "O2 Cloud", stack: [ROOT], active: state.stack.length === 1 },
  ];
  if (state.settings.group_camera) {
    places.push({
      icon: "📷",
      label: "Cámara",
      stack: [ROOT, CAMERA],
      active: top?.virtual?.startsWith("camera") ?? false,
    });
  }
  for (const folder of state.rootFolders) {
    places.push({
      icon: "📁",
      label: folder.name,
      stack: [ROOT, { id: folder.id, name: folder.name, path: joinPath("/", folder.name) }],
      active: top?.id === folder.id,
    });
  }

  for (const place of places) {
    const button = document.createElement("button");
    button.className = `place${place.active ? " active" : ""}`;
    button.title = place.label;
    const icon = document.createElement("span");
    icon.className = "place-icon";
    icon.textContent = place.icon;
    const label = document.createElement("span");
    label.className = "place-label";
    label.textContent = place.label;
    button.append(icon, label);
    button.addEventListener("click", () => navigate(place.stack));
    nav.appendChild(button);
  }
}

function renderPathbar() {
  const nav = $("#breadcrumbs");
  nav.replaceChildren();
  state.stack.forEach((crumb, index) => {
    if (index > 0) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "›";
      nav.appendChild(sep);
    }
    const button = document.createElement("button");
    button.className = "crumb";
    button.textContent = crumb.name;
    button.title = crumb.name;
    if (index === state.stack.length - 1) button.classList.add("current");
    button.addEventListener("click", () => navigate(state.stack.slice(0, index + 1)));
    nav.appendChild(button);
  });
  ($("#back-btn") as HTMLButtonElement).disabled = state.stack.length === 1;
  const crumb = currentCrumb();
  $("#drop-text").textContent = `Suelta para subir a ${crumb.virtual ? "O2 Cloud" : crumb.name}`;
}

function navigate(stack: Crumb[]) {
  state.stack = stack;
  state.selected.clear();
  state.anchor = null;
  state.cursor = null;
  state.filter = "";
  ($("#search") as HTMLInputElement).value = "";
  hideMenu();
  void refresh();
}

function goUp() {
  if (state.stack.length > 1) navigate(state.stack.slice(0, -1));
}

function openFolder(folder: RemoteFolder) {
  const crumb = currentCrumb();
  navigate([
    ...state.stack,
    { id: folder.id, name: folder.name, path: joinPath(crumb.path, folder.name) },
  ]);
}

function openVirtualFolder(folder: VirtualFolder) {
  navigate([
    ...state.stack,
    { id: null, name: folder.name, path: currentCrumb().path, virtual: folder.key },
  ]);
}

// ---------- Entries: sorting and filtering ----------

const collator = new Intl.Collator("es", { numeric: true, sensitivity: "base" });

function normalize(text: string): string {
  return text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase();
}

function sizeOf(entry: Entry): number {
  return entry.kind === "file" ? entry.file.size : entry.folder.size;
}

function dateOf(entry: Entry): number | null {
  return entry.kind === "file" ? timeOf(entry.file.date) : null;
}

/** By the chosen column; unknown dates last; then by name. */
function compareEntries(a: Entry, b: Entry): number {
  const { key, dir } = state.sort;
  if (key === "size") {
    const diff = sizeOf(a) - sizeOf(b);
    if (diff) return diff * dir;
  } else if (key === "date") {
    const ta = dateOf(a);
    const tb = dateOf(b);
    if (ta !== tb) {
      if (ta == null) return 1;
      if (tb == null) return -1;
      return (ta - tb) * dir;
    }
  }
  return collator.compare(a.name, b.name) * (key === "name" ? dir : 1);
}

function fileKey(file: LogicalFile): string {
  return file.normal_id != null ? `file:${file.normal_id}` : `chunked:${file.name}`;
}

let entriesCache = {
  listing: null as LogicalListing | null,
  signature: "",
  entries: [] as Entry[],
};

/** Virtual folders in their own order, then folders, then files. */
function visibleEntries(): Entry[] {
  const listing = state.listing;
  if (!listing) return [];
  const signature = `${state.sort.key}:${state.sort.dir}:${state.filter}`;
  if (entriesCache.listing === listing && entriesCache.signature === signature) {
    return entriesCache.entries;
  }

  const needle = normalize(state.filter.trim());
  const matches = (name: string) => !needle || normalize(name).includes(needle);
  const virtual: Entry[] = (listing.virtual_folders ?? [])
    .filter((v) => matches(v.name))
    .map((folder): Entry => ({ kind: "virtual", key: `virtual:${folder.key}`, name: folder.name, folder }));
  const folders: Entry[] = listing.folders
    .filter((f) => matches(f.name))
    .map((folder): Entry => ({ kind: "folder", key: `folder:${folder.id}`, name: folder.name, folder }))
    .sort(compareEntries);
  const files: Entry[] = listing.files
    .filter((f) => matches(f.name))
    .map((file): Entry => ({ kind: "file", key: fileKey(file), name: file.name, file }))
    .sort(compareEntries);

  const entries = [...virtual, ...folders, ...files];
  entriesCache = { listing, signature, entries };
  return entries;
}

function selectableKeys(): string[] {
  return visibleEntries()
    .filter((entry) => entry.kind !== "virtual")
    .map((entry) => entry.key);
}

function selectedEntries(): (FolderEntry | FileEntry)[] {
  return visibleEntries().filter(
    (entry): entry is FolderEntry | FileEntry =>
      entry.kind !== "virtual" && state.selected.has(entry.key),
  );
}

// ---------- File list ----------

const rowByKey = new Map<string, HTMLTableRowElement>();

const SVG_NS = "http://www.w3.org/2000/svg";
const ICON_PATHS = {
  download: "M12 4v11m0 0-4-4m4 4 4-4M5 19h14",
  trash: "M4 7h16M10 11v6m4-6v6M9 7V5h6v2M6 7l1 12h10l1-12",
};

function svgIcon(name: keyof typeof ICON_PATHS): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.8");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", ICON_PATHS[name]);
  svg.appendChild(path);
  return svg;
}

const FILE_ICONS: [RegExp, string][] = [
  [/\.(jpe?g|png|heic|heif|gif|webp|tiff?|bmp|dng|raw|cr[23]|nef|arw|avif)$/i, "🖼️"],
  [/\.(mov|mp4|m4v|3gp|3g2|avi|mkv|webm|mts|m2ts|mpe?g|wmv|flv)$/i, "🎬"],
  [/\.(mp3|m4a|flac|wav|aac|ogg|opus|aiff?)$/i, "🎵"],
  [/\.(zip|rar|7z|tar|gz|tgz|bz2|xz|dmg|iso)$/i, "🗜️"],
  [/\.pdf$/i, "📕"],
];

function entryIcon(entry: Entry): string {
  if (entry.kind === "virtual") {
    return entry.folder.kind === "camera" ? "📷" : entry.folder.kind === "year" ? "🗓️" : "🗂️";
  }
  if (entry.kind === "folder") return "📁";
  if (entry.file.chunked) return "🧩";
  return FILE_ICONS.find(([pattern]) => pattern.test(entry.name))?.[1] ?? "📄";
}

function makeAction(
  icon: keyof typeof ICON_PATHS,
  title: string,
  onClick: () => void,
  danger = false,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.className = `row-action${danger ? " danger" : ""}`;
  button.title = title;
  button.setAttribute("aria-label", title);
  button.appendChild(svgIcon(icon));
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  button.addEventListener("dblclick", (event) => event.stopPropagation());
  return button;
}

function cell(className: string): HTMLTableCellElement {
  const td = document.createElement("td");
  td.className = className;
  return td;
}

function renderRow(entry: Entry): HTMLTableRowElement {
  const tr = document.createElement("tr");
  tr.className = `row ${entry.kind}-row`;
  tr.dataset.key = entry.key;
  tr.classList.toggle("selected", state.selected.has(entry.key));
  tr.classList.toggle("trashing", state.trashing.has(entry.key));
  tr.addEventListener("click", (event) => onRowClick(event, entry));
  tr.addEventListener("dblclick", () => openEntry(entry));
  tr.addEventListener("contextmenu", (event) => onRowContextMenu(event, entry));

  const check = cell("col-check");
  if (entry.kind !== "virtual") {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = state.selected.has(entry.key);
    box.setAttribute("aria-label", `Seleccionar ${entry.name}`);
    box.addEventListener("click", (event) => {
      event.stopPropagation();
      toggleSelected(entry.key);
      state.anchor = entry.key;
      state.cursor = entry.key;
      updateSelection();
    });
    box.addEventListener("dblclick", (event) => event.stopPropagation());
    check.appendChild(box);
  }

  const nameCell = cell("col-name");
  const wrap = document.createElement("div");
  wrap.className = "name-wrap";
  const icon = document.createElement("span");
  icon.className = "row-icon";
  icon.textContent = entryIcon(entry);
  const name = document.createElement("span");
  name.className = "name-text";
  name.textContent = entry.name;
  name.title = entry.name;
  wrap.append(icon, name);
  if (entry.kind === "virtual" || (entry.kind === "folder" && entry.folder.files > 0)) {
    const meta = document.createElement("span");
    meta.className = "name-meta";
    meta.textContent = plural(entry.folder.files, "archivo", "archivos");
    wrap.appendChild(meta);
  }
  if (entry.kind === "virtual") {
    const pill = document.createElement("span");
    pill.className = "pill";
    pill.textContent = "virtual";
    pill.title = "Vista de la app: no mueve nada en O2";
    wrap.appendChild(pill);
  }
  if (entry.kind === "file" && entry.file.chunked) {
    const pill = document.createElement("span");
    pill.className = `pill${entry.file.incomplete ? " warning" : ""}`;
    pill.textContent = entry.file.incomplete ? "incompleto" : "trozos";
    pill.title = entry.file.incomplete
      ? "Faltan el manifiesto o algún fragmento"
      : "Archivo dividido automáticamente para superar el límite de 4 GB";
    wrap.appendChild(pill);
  }
  nameCell.appendChild(wrap);

  const sizeCell = cell("col-size");
  sizeCell.textContent = sizeOf(entry) ? formatSize(sizeOf(entry)) : "—";

  const dateCell = cell("col-date");
  dateCell.textContent = entry.kind === "file" ? formatDate(entry.file.date) : "";

  const actionsCell = cell("col-actions");
  const actions = document.createElement("div");
  actions.className = "row-actions";
  if (entry.kind === "file") {
    actions.appendChild(makeAction("download", "Descargar", () => void downloadFiles([entry.file])));
  }
  if (entry.kind !== "virtual") {
    actions.appendChild(
      makeAction("trash", "Mover a la papelera", () => void trashEntries([entry]), true),
    );
  }
  actionsCell.appendChild(actions);

  tr.append(check, nameCell, sizeCell, dateCell, actionsCell);
  return tr;
}

function renderWarnings() {
  const box = $("#warnings");
  const warnings = state.listing?.warnings ?? [];
  box.replaceChildren();
  box.classList.toggle("hidden", !warnings.length);
  if (!warnings.length) return;
  const title = document.createElement("strong");
  title.textContent = "Avisos";
  const ul = document.createElement("ul");
  for (const warning of warnings) {
    const li = document.createElement("li");
    li.textContent = warning;
    ul.appendChild(li);
  }
  box.append(title, ul);
}

function describeListing(entries: Entry[], shown: number): string {
  const listing = state.listing;
  if (!listing) return state.loading ? "Cargando…" : "";
  const folders = entries.filter((e) => e.kind !== "file").length;
  const files = entries.length - folders;
  const parts = [
    folders ? plural(folders, "carpeta", "carpetas") : "",
    files || !folders ? plural(files, "archivo", "archivos") : "",
  ].filter(Boolean);
  if (state.filter.trim()) {
    const total =
      (listing.virtual_folders?.length ?? 0) + listing.folders.length + listing.files.length;
    parts.push(`de ${total.toLocaleString("es-ES")}`);
  }
  if (shown < entries.length) parts.push(`mostrando ${shown.toLocaleString("es-ES")}`);
  if (currentCrumb().virtual) parts.push("vista virtual");
  return parts.join(" · ");
}

function renderSortHeaders() {
  // Virtual folders keep their own order (years newest first).
  const sortable = !!state.listing && (state.listing.folders.length > 0 || state.listing.files.length > 0);
  for (const button of document.querySelectorAll<HTMLButtonElement>(".sort")) {
    const active = sortable && button.dataset.sort === state.sort.key;
    button.classList.toggle("asc", active && state.sort.dir === 1);
    button.classList.toggle("desc", active && state.sort.dir === -1);
  }
}

function renderListing() {
  const listing = state.listing;
  const tbody = $("#file-list");
  tbody.replaceChildren();
  rowByKey.clear();

  $("#loading-state").classList.toggle("hidden", !(state.loading && !listing));
  $("#refresh-line").classList.toggle("hidden", !(state.loading && listing));
  renderSortHeaders();

  const entries = visibleEntries();
  const shown = entries.slice(0, state.visibleLimit);
  for (const entry of shown) {
    const tr = renderRow(entry);
    rowByKey.set(entry.key, tr);
    tbody.appendChild(tr);
  }

  const empty = !!listing && !entries.length && !state.loading;
  $("#empty-state").classList.toggle("hidden", !empty);
  if (empty) {
    const filtering = state.filter.trim();
    $("#empty-title").textContent = filtering
      ? `Nada coincide con «${filtering}».`
      : "Esta carpeta está vacía.";
    $("#empty-hint").textContent = filtering
      ? "Prueba con otro nombre."
      : "Arrastra archivos aquí o usa «Subir».";
  }
  $("#load-more-wrap").classList.toggle("hidden", shown.length >= entries.length);
  $("#folder-subtitle").textContent = describeListing(entries, shown.length);
  updateSelection();
}

// ---------- Selection ----------

function toggleSelected(key: string) {
  if (state.selected.has(key)) state.selected.delete(key);
  else state.selected.add(key);
}

function selectRange(from: string, to: string, additive: boolean) {
  const keys = selectableKeys();
  const i = keys.indexOf(from);
  const j = keys.indexOf(to);
  if (i < 0 || j < 0) {
    state.selected = new Set([to]);
    return;
  }
  const range = keys.slice(Math.min(i, j), Math.max(i, j) + 1);
  state.selected = additive ? new Set([...state.selected, ...range]) : new Set(range);
}

function selectAll() {
  state.selected = new Set(selectableKeys());
  updateSelection();
}

function clearSelection() {
  state.selected.clear();
  state.anchor = null;
  updateSelection();
}

function onRowClick(event: MouseEvent, entry: Entry) {
  if (entry.kind === "virtual") {
    openEntry(entry);
    return;
  }
  const additive = event.metaKey || event.ctrlKey;
  if (event.shiftKey && state.anchor) {
    selectRange(state.anchor, entry.key, additive);
  } else if (additive) {
    toggleSelected(entry.key);
    state.anchor = entry.key;
  } else {
    state.selected = new Set([entry.key]);
    state.anchor = entry.key;
  }
  state.cursor = entry.key;
  updateSelection();
}

/** Arrow keys: move the selection one row; with Shift, extend it. */
function moveSelection(step: number, extend: boolean) {
  const keys = selectableKeys();
  if (!keys.length) return;
  const from = state.cursor ? keys.indexOf(state.cursor) : -1;
  const next =
    from < 0 ? (step > 0 ? 0 : keys.length - 1) : Math.max(0, Math.min(keys.length - 1, from + step));
  const key = keys[next];
  state.cursor = key;
  if (extend && state.anchor) {
    selectRange(state.anchor, key, false);
  } else {
    state.selected = new Set([key]);
    state.anchor = key;
  }
  if (!rowByKey.has(key)) {
    state.visibleLimit += PAGE;
    renderListing();
  } else {
    updateSelection();
  }
  rowByKey.get(key)?.scrollIntoView({ block: "nearest" });
}

/** Reflect the selection on the rows on screen and in the action bar. */
function updateSelection() {
  for (const [key, tr] of rowByKey) {
    const selected = state.selected.has(key);
    tr.classList.toggle("selected", selected);
    const box = tr.querySelector<HTMLInputElement>(".col-check input");
    if (box) box.checked = selected;
  }

  const all = selectableKeys().length;
  const selectAllBox = $("#select-all") as HTMLInputElement;
  selectAllBox.checked = all > 0 && state.selected.size === all;
  selectAllBox.indeterminate = state.selected.size > 0 && state.selected.size < all;
  selectAllBox.disabled = all === 0;

  const entries = selectedEntries();
  $("#selection-bar").classList.toggle("hidden", !entries.length);
  $("#folder-subtitle").classList.toggle("hidden", entries.length > 0);
  if (!entries.length) return;
  const size = entries.reduce((sum, entry) => sum + sizeOf(entry), 0);
  $("#selection-text").textContent = `${plural(entries.length, "seleccionado", "seleccionados")} · ${formatSize(size)}`;
  const files = entries.filter((entry) => entry.kind === "file").length;
  const download = $("#selection-download") as HTMLButtonElement;
  download.disabled = files === 0;
  download.title = files < entries.length ? "Las carpetas no se descargan; solo los archivos" : "";
}

function openEntry(entry: Entry) {
  if (entry.kind === "virtual") openVirtualFolder(entry.folder);
  else if (entry.kind === "folder") openFolder(entry.folder);
  else void downloadFiles([entry.file]);
}

/** Enter: open a single selected folder, or download the selected files. */
function openSelection() {
  const entries = selectedEntries();
  if (entries.length === 1 && entries[0].kind === "folder") {
    openEntry(entries[0]);
    return;
  }
  downloadSelected();
}

function downloadSelected() {
  const files = selectedEntries().filter((entry): entry is FileEntry => entry.kind === "file");
  if (files.length) void downloadFiles(files.map((entry) => entry.file));
}

// ---------- Menus ----------

function showMenu(entries: MenuEntry[], x: number, y: number) {
  const menu = $("#menu");
  menu.replaceChildren();
  for (const entry of entries) {
    if (entry === "separator") {
      const separator = document.createElement("div");
      separator.className = "menu-separator";
      menu.appendChild(separator);
      continue;
    }
    const item = document.createElement("button");
    item.className = `menu-item${entry.danger ? " danger" : ""}`;
    item.setAttribute("role", "menuitem");
    item.disabled = !!entry.disabled;
    const label = document.createElement("span");
    label.textContent = entry.label;
    item.appendChild(label);
    if (entry.hint) {
      const hint = document.createElement("span");
      hint.className = "menu-hint";
      hint.textContent = entry.hint;
      item.appendChild(hint);
    }
    item.addEventListener("click", () => {
      hideMenu();
      entry.run();
    });
    menu.appendChild(item);
  }
  menu.classList.remove("hidden");
  // Keep it on screen.
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - rect.width - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - rect.height - 8))}px`;
}

function hideMenu() {
  $("#menu").classList.add("hidden");
}

function menuOpen(): boolean {
  return !$("#menu").classList.contains("hidden");
}

function moveMenuFocus(step: number) {
  const items = [...document.querySelectorAll<HTMLButtonElement>("#menu .menu-item:not(:disabled)")];
  if (!items.length) return;
  const index = items.indexOf(document.activeElement as HTMLButtonElement);
  items[(index + step + items.length) % items.length].focus();
}

function selectionMenu(): MenuEntry[] {
  const entries = selectedEntries();
  const files = entries.filter((entry): entry is FileEntry => entry.kind === "file");
  const items: MenuEntry[] = [];
  if (entries.length === 1 && entries[0].kind === "folder") {
    const folder = entries[0];
    items.push({ label: "Abrir", hint: "↩", run: () => openEntry(folder) });
  }
  if (files.length) {
    items.push({
      label: files.length === 1 ? "Descargar…" : `Descargar ${files.length} archivos…`,
      hint: items.length ? undefined : "↩",
      run: downloadSelected,
    });
  }
  if (items.length) items.push("separator");
  items.push({
    label: "Mover a la papelera",
    hint: "⌘⌫",
    danger: true,
    run: () => void trashEntries(entries),
  });
  return items;
}

function backgroundMenu(): MenuEntry[] {
  const items: MenuEntry[] = [
    { label: "Subir archivos…", run: () => void chooseFiles(false) },
    { label: "Subir carpeta…", run: () => void chooseFiles(true) },
  ];
  if (state.photos.available) {
    items.push({ label: "Subir desde Fotos…", run: () => void openPhotos() });
  }
  items.push(
    { label: "Nueva carpeta…", hint: "⇧⌘N", run: () => createFolder() },
    "separator",
    { label: "Recargar", hint: "⌘R", run: () => void refresh({ fresh: true }) },
  );
  return items;
}

function onRowContextMenu(event: MouseEvent, entry: Entry) {
  event.preventDefault();
  event.stopPropagation();
  if (entry.kind === "virtual") {
    showMenu([{ label: "Abrir", hint: "↩", run: () => openEntry(entry) }], event.clientX, event.clientY);
    return;
  }
  if (!state.selected.has(entry.key)) {
    state.selected = new Set([entry.key]);
    state.anchor = entry.key;
    state.cursor = entry.key;
    updateSelection();
  }
  showMenu(selectionMenu(), event.clientX, event.clientY);
}

// ---------- Transfers ----------

interface QueueRow {
  row: HTMLDivElement;
  name: HTMLSpanElement;
  message: HTMLSpanElement;
  fill: HTMLDivElement;
  detail: HTMLDivElement;
}

// Rows are updated in place, so the bar animates between updates and an
// indeterminate bar doesn't restart on every event.
const queueRows = new Map<string, QueueRow>();

/** Smoothed speed of each running transfer, in bytes per second. */
const transferRates = new Map<string, { bytes: number; time: number; rate: number }>();

function isFinished(item: TransferProgress): boolean {
  return item.phase === "done" || item.phase === "error";
}

function trackRate(item: TransferProgress) {
  if (isFinished(item)) {
    transferRates.delete(item.id);
    return;
  }
  const now = performance.now();
  const last = transferRates.get(item.id);
  if (!last) {
    transferRates.set(item.id, { bytes: item.bytes_done, time: now, rate: 0 });
    return;
  }
  // Back to the start: a failed attempt is being tried again.
  if (item.bytes_done < last.bytes) {
    transferRates.set(item.id, { bytes: item.bytes_done, time: now, rate: 0 });
    return;
  }
  const seconds = (now - last.time) / 1000;
  if (item.bytes_done <= last.bytes || seconds < 0.2) return;
  const instant = (item.bytes_done - last.bytes) / seconds;
  const rate = last.rate ? last.rate * 0.7 + instant * 0.3 : instant;
  transferRates.set(item.id, { bytes: item.bytes_done, time: now, rate });
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

/** How far along a transfer is, or null while nothing can be measured yet. */
function progressRatio(item: TransferProgress): number | null {
  if (isFinished(item)) return 1;
  if (item.phase === "queued") return 0;
  if (item.bytes_total > 0 && item.bytes_done > 0) {
    return Math.min(1, item.bytes_done / item.bytes_total);
  }
  if (item.total > 0 && item.current > 0) return Math.min(1, item.current / item.total);
  return null;
}

function transferDetail(item: TransferProgress): string {
  if (item.phase === "error" || item.bytes_total <= 0) return "";
  if (item.phase === "done" || item.phase === "queued") return formatSize(item.bytes_total);
  const parts = [`${formatSize(item.bytes_done)} de ${formatSize(item.bytes_total)}`];
  const rate = transferRates.get(item.id)?.rate ?? 0;
  // Nothing left to send (O2 is storing it): a speed would only go stale.
  if (rate > 0 && item.bytes_done < item.bytes_total) {
    parts.push(`${formatSize(rate)}/s`);
    const left = (item.bytes_total - item.bytes_done) / rate;
    if (left >= 1) parts.push(`quedan ${formatDuration(left)}`);
  }
  return parts.join(" · ");
}

function createQueueRow(): QueueRow {
  const row = document.createElement("div");
  const top = document.createElement("div");
  top.className = "queue-item-top";
  const name = document.createElement("span");
  name.className = "queue-name";
  const message = document.createElement("span");
  message.className = "queue-message";
  top.append(name, message);

  const bar = document.createElement("div");
  bar.className = "progress";
  const fill = document.createElement("div");
  fill.className = "progress-fill";
  bar.appendChild(fill);

  const detail = document.createElement("div");
  detail.className = "queue-detail";
  row.append(top, bar, detail);
  return { row, name, message, fill, detail };
}

function updateQueueRow(view: QueueRow, item: TransferProgress) {
  view.row.className = `queue-item ${item.phase}`;
  const icon =
    item.kind === "download"
      ? "↓"
      : item.kind === "delete"
        ? "⌫"
        : item.kind === "library"
          ? "☁"
          : item.kind === "cli"
            ? "⚙"
            : "↑";
  view.name.textContent = `${icon} ${item.name}`;
  view.name.title = item.name;
  // Errors can be long: the first line is shown, all of it on hover.
  view.message.textContent = item.message.split("\n")[0];
  view.message.title = item.message;

  const ratio = progressRatio(item);
  view.fill.classList.toggle("indeterminate", ratio === null);
  view.fill.classList.toggle("done", item.phase === "done");
  view.fill.classList.toggle("error", item.phase === "error");
  view.fill.style.width = ratio === null ? "" : `${Math.round(ratio * 100)}%`;

  const detail = transferDetail(item);
  view.detail.textContent = detail;
  view.detail.classList.toggle("hidden", !detail);
}

/** The toolbar button: how many files are left, and how far the transfers are overall. */
function renderTransfersButton(filesLeft: number) {
  const active = [...state.queue.values()].filter((item) => !isFinished(item));
  const count = $("#transfers-count");
  count.textContent = String(filesLeft);
  count.classList.toggle("hidden", !filesLeft);
  $("#transfers-btn").classList.toggle("active", filesLeft > 0);

  const measured = active.filter((item) => item.bytes_total > 0);
  const total = measured.reduce((sum, item) => sum + item.bytes_total, 0);
  const done = measured.reduce((sum, item) => sum + Math.min(item.bytes_done, item.bytes_total), 0);
  $("#transfers-progress").classList.toggle("hidden", !total);
  if (total) $("#transfers-progress-fill").style.width = `${Math.round((done / total) * 100)}%`;
}

/** Finished transfers remembered at most. */
const MAX_FINISHED = 100;
/** Rows the transfers popover shows at once. */
const MAX_TRANSFER_ROWS = 50;
/** Queued transfers listed before the rest are summed up in one line. */
const QUEUED_ROWS = 3;

/**
 * Keep the queue bounded during long uploads by forgetting the oldest
 * finished transfers, those that went well before those that failed.
 * Queued and running ones stay, however many: the toolbar counts them.
 */
function forgetOldTransfers() {
  let finished = [...state.queue.values()].filter(isFinished).length;
  for (const phase of ["done", "error"]) {
    for (const [id, item] of state.queue) {
      if (finished <= MAX_FINISHED) return;
      if (item.phase === phase) {
        state.queue.delete(id);
        transferRates.delete(id);
        finished--;
      }
    }
  }
}

/**
 * For a file of a folder upload, the folder's transfer: the backend names
 * those `<folder's ID>/<file's>`.
 */
function folderOf(id: string): string | null {
  const slash = id.indexOf("/");
  return slash < 0 ? null : id.slice(0, slash);
}

/** The transfers to list, and the files of each folder upload, to list under it. */
function groupTransfers(): { top: TransferProgress[]; files: Map<string, TransferProgress[]> } {
  const top: TransferProgress[] = [];
  const files = new Map<string, TransferProgress[]>();
  for (const item of state.queue.values()) {
    const folder = folderOf(item.id);
    if (folder === null || !state.queue.has(folder)) {
      top.push(item);
      continue;
    }
    const siblings = files.get(folder);
    if (siblings) siblings.push(item);
    else files.set(folder, [item]);
  }
  return { top, files };
}

/** Files still to transfer: a folder upload counts those it hasn't finished. */
function countFilesLeft(top: TransferProgress[], files: Map<string, TransferProgress[]>): number {
  return top
    .filter((item) => !isFinished(item))
    .reduce((sum, item) => sum + (files.has(item.id) ? Math.max(1, item.total - item.current) : 1), 0);
}

type QueueLine = { item: TransferProgress; file: boolean } | { queued: number };

/**
 * What the popover lists, top to bottom: the transfers running, each
 * folder upload with the file it is on and those that failed; the next
 * few queued and how many more; then the finished, failed first, newest
 * first. A folder's files that went well aren't listed.
 */
function queueLines(top: TransferProgress[], files: Map<string, TransferProgress[]>): QueueLine[] {
  const lines: QueueLine[] = [];
  const add = (item: TransferProgress) => {
    lines.push({ item, file: false });
    const own = files.get(item.id) ?? [];
    for (const file of own.filter((file) => !isFinished(file))) lines.push({ item: file, file: true });
    for (const file of own.filter((file) => file.phase === "error").reverse()) {
      lines.push({ item: file, file: true });
    }
  };
  const queued = top.filter((item) => item.phase === "queued");
  top.filter((item) => !isFinished(item) && item.phase !== "queued").forEach(add);
  queued.slice(0, QUEUED_ROWS).forEach(add);
  if (queued.length > QUEUED_ROWS) lines.push({ queued: queued.length - QUEUED_ROWS });
  top.filter((item) => item.phase === "error").reverse().forEach(add);
  top.filter((item) => item.phase === "done").reverse().forEach(add);
  return lines.slice(0, MAX_TRANSFER_ROWS);
}

const queueEmpty = Object.assign(document.createElement("div"), {
  className: "queue-empty",
  textContent: "No hay transferencias ni operaciones.",
});
const queueMore = Object.assign(document.createElement("div"), { className: "queue-more" });

function renderQueue() {
  const list = $("#queue-list");
  const { top, files } = groupTransfers();
  const lines = queueLines(top, files);

  const shown = new Set(lines.flatMap((line) => ("item" in line ? [line.item.id] : [])));
  for (const [id, view] of queueRows) {
    if (!shown.has(id)) {
      view.row.remove();
      queueRows.delete(id);
    }
  }
  ($("#transfers-clear") as HTMLButtonElement).disabled = !lines.some(
    (line) => "item" in line && isFinished(line.item),
  );
  renderTransfersButton(countFilesLeft(top, files));

  const elements = lines.map((line) => {
    if (!("item" in line)) {
      queueMore.textContent = `Y ${line.queued} más en cola`;
      return queueMore;
    }
    let view = queueRows.get(line.item.id);
    if (!view) {
      view = createQueueRow();
      queueRows.set(line.item.id, view);
    }
    updateQueueRow(view, line.item);
    view.row.classList.toggle("file", line.file);
    return view.row;
  });
  if (!elements.length) elements.push(queueEmpty);
  // In order, moving only what is out of place.
  elements.forEach((element, index) => {
    if (list.children[index] !== element) list.insertBefore(element, list.children[index] ?? null);
  });
  while (list.children.length > elements.length) list.lastElementChild?.remove();
}

function clearFinishedTransfers() {
  for (const [id, item] of state.queue) {
    if (isFinished(item)) {
      state.queue.delete(id);
      transferRates.delete(id);
    }
  }
  renderQueue();
}

/** Show a transfer in the queue as soon as it is asked for. */
function queueTransfer(kind: "upload" | "download", name: string, size = 0): string {
  // randomUUID only exists in secure contexts.
  const id = crypto.randomUUID?.() ?? `t-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  state.queue.set(id, {
    id,
    kind,
    name,
    phase: "queued",
    current: 0,
    total: 0,
    bytes_done: 0,
    bytes_total: size,
    message: "En cola",
  });
  return id;
}

/**
 * A long operation that is not a file transfer (delete, library sync, CLI
 * update). It shares the queue popover so every pending action is visible.
 */
function queueOperation(
  kind: "delete" | "library" | "cli",
  name: string,
  message: string,
  open = false,
): string {
  const id = crypto.randomUUID?.() ?? `op-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  state.queue.set(id, {
    id,
    kind,
    name,
    phase: "running",
    current: 0,
    total: 0,
    bytes_done: 0,
    bytes_total: 0,
    message,
  });
  renderQueue();
  if (open) openTransfers();
  return id;
}

function finishOperation(id: string, message: string, failed = false) {
  const item = state.queue.get(id);
  if (!item) return;
  state.queue.set(id, {
    ...item,
    phase: failed ? "error" : "done",
    message,
  });
  renderQueue();
}

/** Rows still queued when their batch ends never started: say so. */
function failQueued(ids: string[], message: string) {
  for (const id of ids) {
    const item = state.queue.get(id);
    if (item?.phase === "queued") state.queue.set(id, { ...item, phase: "error", message });
  }
  renderQueue();
}

function transfersOpen(): boolean {
  return !$("#transfers-popover").classList.contains("hidden");
}

function openTransfers() {
  const popover = $("#transfers-popover");
  const button = $("#transfers-btn");
  const rect = button.getBoundingClientRect();
  popover.style.top = `${rect.bottom + 8}px`;
  popover.style.right = `${Math.max(12, window.innerWidth - rect.right)}px`;
  popover.classList.remove("hidden");
  button.setAttribute("aria-expanded", "true");
}

function closeTransfers() {
  $("#transfers-popover").classList.add("hidden");
  $("#transfers-btn").setAttribute("aria-expanded", "false");
}

// ---------- Loading ----------

// A listing can take several seconds; only the latest one may be shown.
let refreshSeq = 0;
/** The view `state.listing` belongs to. */
let listingView = "";

// ---------- Mutations and cached state ----------

/** How long a local change is trusted while O2's listing catches up. */
const MUTATION_GRACE_MS = 60_000;
/** A delete can wait for several whole-account CLI listings. */
const REMOVAL_GRACE_MS = 5 * 60_000;

/** Folders created here but not yet present in O2's listing, by view. */
const pendingFolderAdds = new Map<
  string,
  Map<number, { folder: RemoteFolder; until: number }>
>();
/** Entries deleted here but possibly still present in O2's listing, by view. */
const pendingRemovals = new Map<string, { keys: Set<string>; until: number }>();

function rememberFolderAdd(view: string, folder: RemoteFolder) {
  let folders = pendingFolderAdds.get(view);
  if (!folders) {
    folders = new Map();
    pendingFolderAdds.set(view, folders);
  }
  folders.set(folder.id, {
    folder,
    until: Date.now() + MUTATION_GRACE_MS,
  });
}

function rememberRemovals(view: string, keys: Set<string>) {
  let pending = pendingRemovals.get(view);
  if (!pending) {
    pending = { keys: new Set(), until: 0 };
    pendingRemovals.set(view, pending);
  }
  for (const key of keys) pending.keys.add(key);
  pending.until = Date.now() + REMOVAL_GRACE_MS;
}

/** Undo an optimistic removal when O2 rejects the delete. */
function forgetRemovals(view: string, keys: Set<string>) {
  const pending = pendingRemovals.get(view);
  if (!pending) return;
  for (const key of keys) pending.keys.delete(key);
  if (pending.keys.size === 0) pendingRemovals.delete(view);
}

/**
 * O2 lists writes and deletes with a delay, and a refresh can overlap with a
 * mutation. Keep showing what the user just did until the server agrees.
 */
function applyPendingMutations(view: string, listing: LogicalListing): LogicalListing {
  const now = Date.now();

  const adds = pendingFolderAdds.get(view);
  if (adds) {
    for (const [id, pending] of adds) {
      if (pending.until <= now) {
        adds.delete(id);
        continue;
      }
      if (listing.folders.some((folder) => folder.id === id)) {
        // The server listing already reflects the creation.
        adds.delete(id);
      } else {
        listing.folders.push(pending.folder);
      }
    }
    if (adds.size === 0) pendingFolderAdds.delete(view);
  }

  const removals = pendingRemovals.get(view);
  if (removals) {
    if (removals.until <= now) {
      pendingRemovals.delete(view);
    } else {
      listing.folders = listing.folders.filter(
        (folder) => !removals.keys.has(`folder:${folder.id}`),
      );
      listing.files = listing.files.filter(
        (file) => !removals.keys.has(fileKey(file)),
      );
    }
  }

  return listing;
}

/** Remove deleted rows from the current view without waiting for O2. */
function removeEntriesFromView(view: string, keys: Set<string>) {
  if (listingView !== view || !state.listing) return;
  state.listing.folders = state.listing.folders.filter(
    (folder) => !keys.has(`folder:${folder.id}`),
  );
  state.listing.files = state.listing.files.filter(
    (file) => !keys.has(fileKey(file)),
  );
  entriesCache.listing = null;
  if (state.stack.length === 1 && !currentCrumb().virtual) {
    state.rootFolders = state.listing.folders;
  }
  renderListing();
  renderPlaces();
}

/** List the current folder. `fresh` skips the backend's listing cache. */
async function refresh(options: { fresh?: boolean } = {}) {
  const seq = ++refreshSeq;
  const crumb = currentCrumb();
  const view = crumbKey(crumb);

  // A folder created in this session is empty. Show it at once instead of
  // paying for another `ls` just to confirm what we already know. The marker
  // expires so an externally modified folder is not hidden for long.
  if (options.fresh && crumb.id != null) {
    knownEmptyFolders.delete(crumb.id);
  }
  const emptyUntil = crumb.id != null ? knownEmptyFolders.get(crumb.id) : undefined;
  if (emptyUntil != null && emptyUntil <= Date.now()) {
    knownEmptyFolders.delete(crumb.id!);
  }
  if (
    !options.fresh &&
    crumb.id != null &&
    !crumb.virtual &&
    (knownEmptyFolders.get(crumb.id) ?? 0) > Date.now()
  ) {
    state.listing = {
      path: crumb.path,
      folders: [],
      files: [],
      warnings: [],
      virtual_folders: [],
    };
    listingView = view;
    state.loading = false;
    state.visibleLimit = PAGE;
    $("#table-wrap").scrollTop = 0;
    renderPathbar();
    renderPlaces();
    renderListing();
    return;
  }

  if (view !== listingView) {
    // Another folder: don't show the previous one's files meanwhile.
    state.listing = null;
    state.visibleLimit = PAGE;
    $("#table-wrap").scrollTop = 0;
  }
  state.loading = true;
  renderPathbar();
  renderPlaces();
  renderListing();
  try {
    const listing = await invoke<LogicalListing>("list_dir", {
      folderId: crumb.id,
      path: crumb.path,
      virtualPath: crumb.virtual ?? null,
      fresh: options.fresh ?? false,
    });
    if (seq !== refreshSeq) return;
    state.listing = applyPendingMutations(view, listing);
    listingView = view;
    if (listing.path && listing.path !== crumb.path && listing.path !== "/") {
      crumb.path = listing.path;
    }
    if (state.stack.length === 1) state.rootFolders = listing.folders;
    // Forget selected rows that are gone.
    const keys = new Set(visibleEntries().map((entry) => entry.key));
    state.selected = new Set([...state.selected].filter((key) => keys.has(key)));
    renderWarnings();
    renderPlaces();
    // With an expired session the CLI shows its own login window, so a
    // listing can succeed while we think there is no session.
    if (!state.session?.logged_in) void refreshSession(false);
    if (options.fresh) void refreshQuota();
  } catch (error) {
    if (seq !== refreshSeq) return;
    reportError(error, 9000);
    state.listing = {
      path: crumb.path,
      folders: [],
      files: [],
      warnings: [String(error)],
      virtual_folders: [],
    };
    listingView = view;
    renderWarnings();
  } finally {
    if (seq === refreshSeq) {
      state.loading = false;
      renderListing();
    }
  }
}

/** Check the O2 session. Close the login window if it's active; open it if not, unless `prompt` is false. */
async function refreshSession(prompt = true) {
  try {
    state.session = await invoke<SessionStatus>("get_session_status");
  } catch (error) {
    state.session = {
      logged_in: false,
      raw: String(error),
      phone: null,
      storage: null,
    };
  }
  renderSession();
  if (state.session.logged_in) {
    hideLoginModal();
  } else if (prompt) {
    showLoginModal();
    setLoginStatus("Inicia sesión para acceder a tus archivos.");
  }
}

// ---------- Actions ----------

function reportUploadSummary(summary: UploadSummary) {
  const skipped = summary.skipped
    ? ` ${summary.skipped} ya estaba${summary.skipped === 1 ? "" : "n"} en O2.`
    : "";
  if (summary.failed.length) {
    toast(
      `Subidos ${summary.uploaded}. Fallaron ${summary.failed.length}.${skipped} ${summary.failed[0]}`,
      "error",
      10000,
    );
    console.error(summary.failed);
  } else {
    toast(
      `Subidos ${summary.uploaded} archivo${summary.uploaded === 1 ? "" : "s"}.${skipped} O2 puede tardar unos segundos en mostrarlos.`,
      "success",
    );
  }
  if (summary.library_warning) {
    toast(summary.library_warning, "error", 12000);
  }
}

async function uploadPaths(paths: string[], knownHashes?: (string | null)[]) {
  if (!paths.length) return;
  if (!state.session?.logged_in) {
    showLoginModal();
    return;
  }
  const crumb = currentCrumb();
  const view = crumbKey(crumb);
  if (crumb.id != null) knownEmptyFolders.delete(crumb.id);
  const refreshIfStillHere = () => {
    if (crumbKey(currentCrumb()) === view) void refresh({ fresh: true });
  };
  // The backend runs uploads one at a time, in order; until a path's turn
  // comes it shows here as queued.
  const transferIds = paths.map((path) => queueTransfer("upload", path.split(/[\\/]/).pop() || path));
  renderQueue();
  openTransfers();
  state.uploadsInFlight++;
  try {
    const summary = await invoke<UploadSummary>("upload_paths", {
      paths,
      folderId: crumb.id,
      folderPath: crumb.path,
      transferIds,
      knownHashes: knownHashes ?? null,
    });
    reportUploadSummary(summary);
    await refresh({ fresh: true });
    // O2 is eventually consistent on writes: new objects often take 10–60 s
    // to appear in listings. Refresh a couple of times so the user sees them.
    window.setTimeout(refreshIfStillHere, 5000);
    window.setTimeout(refreshIfStillHere, 20000);
  } catch (error) {
    reportError(error);
  } finally {
    state.uploadsInFlight--;
    failQueued(transferIds, "No se ha subido");
  }
}

/**
 * Hash local photos/videos and ask about files already anywhere in O2 before
 * opening the upload queue. Folders are checked by the normal target-folder
 * rule only.
 */
async function prepareUploadPaths(paths: string[]) {
  if (!paths.length) return;
  if (!state.session?.logged_in) {
    showLoginModal();
    return;
  }
  const crumb = currentCrumb();
  toast("Comprobando duplicados en O2…", "info", 60000);
  let checks: PathCheck[] | null = null;
  try {
    checks = await invoke<PathCheck[]>("library_check_paths", {
      paths,
      folderId: crumb.id,
      folderPath: crumb.path,
    });
  } catch (error) {
    console.error(error);
  }
  if (!checks || checks.length !== paths.length) {
    await uploadPaths(paths);
    return;
  }

  const duplicateIndexes = new Set<number>();
  const duplicates: ConflictItem[] = [];
  checks.forEach((check, index) => {
    if (check.is_file && check.copies.length && !check.in_target) {
      duplicateIndexes.add(index);
      duplicates.push({
        id: String(index),
        name: check.path.split(/[\\/]/).pop() || check.path,
        copies: check.copies,
      });
    }
  });

  let uploadDuplicates = new Set<string>();
  if (duplicates.length) {
    const choice = await askLibraryConflict(duplicates, crumb.virtual ? "/" : crumb.path);
    if (choice.kind === "cancel") return;
    uploadDuplicates = choice.upload;
  }

  const remaining: string[] = [];
  const hashes: (string | null)[] = [];
  paths.forEach((path, index) => {
    if (duplicateIndexes.has(index) && !uploadDuplicates.has(String(index))) return;
    remaining.push(path);
    hashes.push(checks?.[index]?.sha256 ?? null);
  });
  if (!remaining.length) {
    toast("No hay nada que subir: ya estaba todo en O2.", "success");
    return;
  }
  await uploadPaths(remaining, hashes);
}

async function chooseFiles(folder: boolean) {
  const selected = await open({
    multiple: true,
    directory: folder,
    title: folder ? "Elige carpetas para subir" : "Elige archivos para subir",
  });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  await prepareUploadPaths(paths);
}

// ---------- Photos (macOS) ----------

function photoNoun(): { one: string; many: string } {
  if (state.photos.mediaType === "photo") return { one: "foto", many: "fotos" };
  if (state.photos.mediaType === "all") return { one: "elemento", many: "elementos" };
  return { one: "vídeo", many: "vídeos" };
}

function formatClipDuration(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = total % 60;
  const mm = String(minutes).padStart(2, "0");
  const ss = String(rest).padStart(2, "0");
  return hours ? `${hours}:${mm}:${ss}` : `${minutes}:${ss}`;
}

function photoKey(asset: PhotoAsset): string {
  return `${asset.name}\u0000${asset.size}`;
}

/** Names and sizes already complete in the folder being browsed (chunks included). */
function existingPhotoKeys(): Set<string> {
  return new Set(
    (state.listing?.files ?? [])
      .filter((file) => !file.incomplete)
      .map((file) => `${file.name}\u0000${file.size}`),
  );
}

function photoIsHere(asset: PhotoAsset, existing: Set<string>): boolean {
  return asset.size > 0 && existing.has(photoKey(asset));
}

function photoMatch(asset: PhotoAsset): PhotoLibraryMatch | undefined {
  return state.photos.matches.get(asset.id);
}

function photoInTarget(asset: PhotoAsset, existing: Set<string>): boolean {
  return photoIsHere(asset, existing) || photoMatch(asset)?.in_target === true;
}

function visiblePhotos(existing: Set<string>): PhotoAsset[] {
  const needle = state.photos.filter.trim().toLowerCase();
  return state.photos.assets.filter((asset) => {
    if (needle && !asset.name.toLowerCase().includes(needle)) return false;
    if (state.photos.onlyLarge && asset.size < LARGE_FILE) return false;
    if (
      state.photos.hideExisting &&
      (photoInTarget(asset, existing) || (photoMatch(asset)?.copies.length ?? 0) > 0)
    ) {
      return false;
    }
    return true;
  });
}

function updatePhotosSelection() {
  const selected = state.photos.assets.filter((asset) => state.photos.selected.has(asset.id));
  const bytes = selected.reduce((sum, asset) => sum + asset.size, 0);
  $("#photos-selection").textContent = selected.length
    ? `${plural(selected.length, "seleccionado", "seleccionados")} · ${formatSize(bytes)}`
    : "Ninguno seleccionado";
  // Checking the shared library can take a while; it must not block choosing
  // an upload. `uploadPhotos` waits for the check before looking for
  // duplicates in other folders.
  ($("#photos-upload") as HTMLButtonElement).disabled =
    !selected.length || state.photos.loading || state.photos.uploading;
}

function createPhotoRow(
  asset: PhotoAsset,
  here: boolean,
  match?: PhotoLibraryMatch,
): HTMLLabelElement {
  const row = document.createElement("label");
  row.className = "photos-row";
  const copyPaths = [...new Set((match?.copies ?? []).map((copy) => copy.folder_path))];
  row.title = copyPaths.length ? `${asset.name} — ${copyPaths.join(", ")}` : asset.name;

  const check = document.createElement("input");
  check.type = "checkbox";
  check.checked = state.photos.selected.has(asset.id);
  check.addEventListener("change", () => {
    if (check.checked) state.photos.selected.add(asset.id);
    else state.photos.selected.delete(asset.id);
    updatePhotosSelection();
  });

  const icon = document.createElement("span");
  icon.className = "photos-icon";
  icon.textContent = asset.media_type === "video" ? "🎬" : "🖼";

  const name = document.createElement("span");
  name.className = "photos-name";
  name.textContent = asset.name;

  const meta = document.createElement("span");
  meta.className = "photos-meta";
  const details: string[] = [];
  if (asset.date) details.push(formatDate(asset.date));
  if (asset.media_type === "video" && asset.duration > 0) {
    details.push(formatClipDuration(asset.duration));
  }
  meta.textContent = details.join(" · ");

  const size = document.createElement("span");
  size.className = "photos-size";
  size.textContent = asset.size ? formatSize(asset.size) : "—";

  const badge = document.createElement("span");
  badge.className = "photos-badge";
  if (here) {
    badge.textContent = "Ya aquí";
  } else if (match?.copies.length) {
    badge.textContent = "Ya en O2";
    badge.classList.add("elsewhere");
  } else {
    badge.classList.add("hidden");
  }
  badge.title = copyPaths.join("\n");

  row.append(check, icon, name, meta, size, badge);
  return row;
}

function renderPhotos() {
  const existing = existingPhotoKeys();
  const visible = visiblePhotos(existing);
  const noun = photoNoun();
  const totalBytes = visible.reduce((sum, asset) => sum + asset.size, 0);
  const onO2 = visible.filter(
    (asset) => photoInTarget(asset, existing) || (photoMatch(asset)?.copies.length ?? 0) > 0,
  ).length;

  $("#photos-loading").classList.toggle("hidden", !state.photos.loading);
  $("#photos-empty").classList.toggle("hidden", state.photos.loading || visible.length > 0);

  const library = state.photos.syncingLibrary
    ? " · sincronizando biblioteca…"
    : state.libraryStats
      ? ` · biblioteca: ${plural(state.libraryStats.files, "archivo", "archivos")}`
      : "";
  $("#photos-summary").textContent = state.photos.loading
    ? "Leyendo Fotos…"
    : `${plural(visible.length, noun.one, noun.many)} · ${formatSize(totalBytes)}${
        onO2 ? ` · ${onO2} ya en O2` : ""
      }${state.photos.truncated ? " · solo se han leído los primeros 10.000" : ""}${library}`;

  const shown = visible.slice(0, state.photos.visibleLimit);
  $("#photos-list").replaceChildren(
    ...shown.map((asset) =>
      createPhotoRow(asset, photoInTarget(asset, existing), photoMatch(asset)),
    ),
  );
  $("#photos-more-wrap").classList.toggle("hidden", visible.length <= shown.length);
  const pending = visible.filter(
    (asset) => !photoInTarget(asset, existing) && !(photoMatch(asset)?.copies.length ?? 0),
  ).length;
  ($("#photos-select-pending") as HTMLButtonElement).disabled =
    pending === 0 || state.photos.syncingLibrary;
  ($("#photos-select-all") as HTMLButtonElement).disabled = visible.length === 0;
  ($("#photos-select-none") as HTMLButtonElement).disabled = state.photos.selected.size === 0;
  updatePhotosSelection();

  if (!state.photos.loading && visible.length === 0) {
    $("#photos-empty-title").textContent =
      state.photos.assets.length === 0
        ? "No hay elementos en Fotos."
        : "No hay elementos que coincidan con el filtro.";
  }
}

function hidePhotos() {
  $("#photos-modal").classList.add("hidden");
}

interface ConflictItem {
  id: string;
  name: string;
  copies: LibraryCopy[];
}

type ConflictChoice =
  | { kind: "cancel" }
  | { kind: "continue"; upload: Set<string> };

let conflictResolve: ((choice: ConflictChoice) => void) | null = null;

/** Ask, file by file, which duplicates to upload again. */
function askLibraryConflict(
  items: ConflictItem[],
  destination: string,
): Promise<ConflictChoice> {
  if (conflictResolve) conflictResolve({ kind: "cancel" });
  const where = destination === "/" ? "la raíz de O2 Cloud" : destination;
  $("#library-conflict-text").textContent =
    items.length === 1
      ? `«${items[0].name}» ya está en O2 en otra carpeta. Marca «Subir copia» si quieres subir otra copia a ${where}; si no, se saltará.`
      : `${items.length} elementos ya están en O2 en otras carpetas. Marca cuáles quieres subir de nuevo a ${where}; los demás se saltarán.`;

  const list = $("#library-conflict-list");
  list.replaceChildren();
  for (const item of items) {
    const row = document.createElement("label");
    row.className = "conflict-item";

    const check = document.createElement("input");
    check.type = "checkbox";
    check.dataset.id = item.id;
    check.checked = false;

    const copy = document.createElement("div");
    copy.className = "conflict-copy";
    const name = document.createElement("strong");
    name.textContent = item.name;
    const paths = document.createElement("span");
    paths.textContent = [...new Set(item.copies.map((item) => item.folder_path))].join(", ");
    copy.append(name, paths);

    row.append(check, copy);
    list.appendChild(row);
  }

  $("#library-conflict-modal").classList.remove("hidden");
  return new Promise((resolve) => {
    conflictResolve = resolve;
  });
}

function setAllConflictChoices(checked: boolean) {
  for (const box of $("#library-conflict-list").querySelectorAll<HTMLInputElement>(
    'input[type="checkbox"]',
  )) {
    box.checked = checked;
  }
}

function finishLibraryConflict(kind: "cancel" | "continue") {
  const upload = new Set<string>();
  if (kind === "continue") {
    for (const box of $("#library-conflict-list").querySelectorAll<HTMLInputElement>(
      'input[type="checkbox"]:checked',
    )) {
      if (box.dataset.id) upload.add(box.dataset.id);
    }
  }
  $("#library-conflict-modal").classList.add("hidden");
  const resolve = conflictResolve;
  conflictResolve = null;
  resolve?.(kind === "cancel" ? { kind: "cancel" } : { kind: "continue", upload });
}

function showPhotosBrowser() {
  $("#photos-permission").classList.add("hidden");
  $("#photos-browser").classList.remove("hidden");
}

function showPhotosPermission(status: string) {
  state.photos.status = status;
  $("#photos-browser").classList.add("hidden");
  $("#photos-permission").classList.remove("hidden");
  const text = $("#photos-permission-text");
  const button = $("#photos-grant") as HTMLButtonElement;
  button.disabled = false;

  if (status === "not_determined") {
    text.textContent =
      "O2 Cloud necesita permiso para leer tu biblioteca y subir los originales, también los que solo están en iCloud.";
    button.textContent = "Permitir acceso a Fotos";
    button.classList.remove("hidden");
  } else if (status === "denied") {
    text.textContent =
      "Has denegado el acceso a Fotos. Actívalo en Ajustes del Sistema › Privacidad y seguridad › Fotos y vuelve a comprobarlo.";
    button.textContent = "Volver a comprobar";
    button.classList.remove("hidden");
  } else if (status === "restricted") {
    text.textContent =
      "El acceso a Fotos está restringido en este Mac (por ejemplo, por gestión de dispositivos).";
    button.classList.add("hidden");
  } else {
    text.textContent =
      "No se ha podido comprobar el acceso a Fotos. Si acabas de cambiarlo en Ajustes del Sistema, vuelve a comprobarlo.";
    button.textContent = "Volver a comprobar";
    button.classList.remove("hidden");
  }
}

// Changing the media type can start a second listing; only the latest may
// replace the rows.
let photosSeq = 0;

async function loadPhotos() {
  const seq = ++photosSeq;
  state.photos.loading = true;
  state.photos.syncingLibrary = true;
  state.photos.matchPromise = null;
  state.photos.assets = [];
  state.photos.matches.clear();
  state.photos.visibleLimit = PHOTO_PAGE;
  renderPhotos();

  const libraryPromise = invoke<LibraryStats>("library_sync", { force: false })
    .then((stats) => {
      state.libraryStats = stats;
    })
    .catch((error) => {
      console.error(error);
    });

  const listPromise = (async () => {
    try {
      const list = await invoke<PhotoList>("photos_list", {
        mediaType: state.photos.mediaType,
        minSize: 0,
      });
      if (seq !== photosSeq) return;
      state.photos.assets = list.assets;
      state.photos.total = list.total;
      state.photos.truncated = list.truncated;
    } catch (error) {
      if (seq !== photosSeq) return;
      state.photos.assets = [];
      state.photos.total = 0;
      state.photos.truncated = false;
      reportError(error);
    } finally {
      if (seq === photosSeq) {
        state.photos.loading = false;
        renderPhotos();
      }
    }
  })();

  // This runs in the background: the list is usable as soon as the assets
  // arrive, and the upload button only waits for it when it is pressed.
  const matchPromise = (async () => {
    await Promise.all([libraryPromise, listPromise]);
    if (seq !== photosSeq) return;
    try {
      const crumb = currentCrumb();
      const matches = await invoke<PhotoLibraryMatch[]>("library_match_photos", {
        assets: state.photos.assets,
        folderId: crumb.id,
        folderPath: crumb.path,
      });
      if (seq !== photosSeq) return;
      state.photos.matches = new Map(matches.map((match) => [match.asset_id, match]));
    } catch (error) {
      if (seq !== photosSeq) return;
      console.error(error);
    } finally {
      if (seq === photosSeq) {
        state.photos.syncingLibrary = false;
        renderPhotos();
      }
    }
  })();

  state.photos.matchPromise = matchPromise;
  await matchPromise;
}

async function openPhotos() {
  if (!state.photos.available) {
    toast("Subir desde Fotos solo está disponible en macOS.", "info");
    return;
  }
  if (!state.session?.logged_in) {
    showLoginModal();
    return;
  }

  const crumb = currentCrumb();
  const destination = crumb.virtual ? "/" : crumb.path;
  $("#photos-where").textContent =
    destination === "/" ? "Se subirán a la raíz de O2 Cloud." : `Se subirán a ${destination}.`;
  state.photos.assets = [];
  state.photos.total = 0;
  state.photos.truncated = false;
  state.photos.selected.clear();
  state.photos.matchPromise = null;
  state.photos.filter = "";
  state.photos.onlyLarge = false;
  state.photos.hideExisting = false;
  state.photos.visibleLimit = PHOTO_PAGE;
  ($("#photos-search") as HTMLInputElement).value = "";
  ($("#photos-only-large") as HTMLInputElement).checked = false;
  ($("#photos-hide-existing") as HTMLInputElement).checked = false;
  ($("#photos-media") as HTMLSelectElement).value = state.photos.mediaType;

  $("#photos-modal").classList.remove("hidden");
  $("#photos-permission").classList.add("hidden");
  $("#photos-browser").classList.add("hidden");

  try {
    const access = await invoke<PhotosAccess>("photos_access_status");
    state.photos.available = access.available;
    state.photos.status = access.status;
    if (!access.available) {
      hidePhotos();
      toast("Subir desde Fotos solo está disponible en macOS.", "info");
      return;
    }
    if (access.status === "authorized" || access.status === "limited") {
      if (access.status === "limited") {
        $("#photos-where").textContent +=
          " Acceso limitado: solo los elementos elegidos en Ajustes del Sistema.";
      }
      showPhotosBrowser();
      await loadPhotos();
    } else {
      showPhotosPermission(access.status);
    }
  } catch (error) {
    hidePhotos();
    reportError(error);
  }
}

async function handlePhotosPermission() {
  const button = $("#photos-grant") as HTMLButtonElement;
  const previous = button.textContent;
  button.disabled = true;
  button.textContent = "Comprobando…";
  try {
    const access =
      state.photos.status === "not_determined"
        ? await invoke<PhotosAccess>("photos_request_access")
        : await invoke<PhotosAccess>("photos_access_status");
    state.photos.available = access.available;
    state.photos.status = access.status;
    if (access.status === "authorized" || access.status === "limited") {
      showPhotosBrowser();
      await loadPhotos();
    } else {
      showPhotosPermission(access.status);
    }
  } catch (error) {
    reportError(error);
    showPhotosPermission(state.photos.status);
  } finally {
    button.disabled = false;
    if (button.textContent === "Comprobando…") button.textContent = previous;
  }
}

function selectPendingPhotos() {
  const existing = existingPhotoKeys();
  state.photos.selected.clear();
  for (const asset of visiblePhotos(existing)) {
    if (!photoInTarget(asset, existing) && !photoMatch(asset)?.copies.length) {
      state.photos.selected.add(asset.id);
    }
  }
  renderPhotos();
}

function selectAllPhotos() {
  state.photos.selected.clear();
  for (const asset of visiblePhotos(existingPhotoKeys())) {
    state.photos.selected.add(asset.id);
  }
  renderPhotos();
}

function clearPhotoSelection() {
  state.photos.selected.clear();
  renderPhotos();
}

function photosUploadSelected() {
  const assets = state.photos.assets.filter((asset) => state.photos.selected.has(asset.id));
  if (assets.length) void uploadPhotos(assets);
}

async function uploadPhotos(assets: PhotoAsset[]) {
  if (!assets.length || state.photos.uploading) return;
  if (!state.session?.logged_in) {
    hidePhotos();
    showLoginModal();
    return;
  }

  const uploadButton = $("#photos-upload") as HTMLButtonElement;
  const uploadLabel = uploadButton.textContent || "Subir seleccionados";
  state.photos.uploading = true;
  updatePhotosSelection();

  try {
    // The library check runs in the background; wait for it so the
    // "already in O2" question is based on the latest shared snapshots.
    if (state.photos.syncingLibrary) {
      uploadButton.textContent = "Comprobando duplicados…";
      if (state.photos.matchPromise) await state.photos.matchPromise;
      uploadButton.textContent = uploadLabel;
    }

    const crumb = currentCrumb();
    if (crumb.id != null) knownEmptyFolders.delete(crumb.id);
    const destination = crumb.virtual ? "/" : crumb.path;
    const duplicates = assets.filter((asset) => {
      const match = photoMatch(asset);
      return match && !match.in_target && match.copies.length > 0;
    });
    const duplicateIds = new Set(duplicates.map((asset) => asset.id));
    let toUpload = assets;
    if (duplicates.length) {
      const choice = await askLibraryConflict(
        duplicates.map((asset) => ({
          id: asset.id,
          name: asset.name,
          copies: photoMatch(asset)?.copies ?? [],
        })),
        destination,
      );
      if (choice.kind === "cancel") return;
      toUpload = assets.filter(
        (asset) => !duplicateIds.has(asset.id) || choice.upload.has(asset.id),
      );
    }
    if (!toUpload.length) {
      hidePhotos();
      toast("No hay nada que subir: ya estaba todo en O2.", "success");
      return;
    }

    const view = crumbKey(crumb);
    const refreshIfStillHere = () => {
      if (crumbKey(currentCrumb()) === view) void refresh({ fresh: true });
    };
    const transferIds = toUpload.map((asset) =>
      queueTransfer("upload", asset.name, asset.size),
    );
    hidePhotos();
    renderQueue();
    openTransfers();
    state.uploadsInFlight++;
    try {
      const summary = await invoke<UploadSummary>("upload_photos", {
        assetIds: toUpload.map((asset) => asset.id),
        folderId: crumb.id,
        folderPath: crumb.path,
        transferIds,
      });
      reportUploadSummary(summary);
      await refresh({ fresh: true });
      // O2 is eventually consistent on writes; refresh a couple more times.
      window.setTimeout(refreshIfStillHere, 5000);
      window.setTimeout(refreshIfStillHere, 20000);
    } catch (error) {
      reportError(error);
    } finally {
      state.uploadsInFlight--;
      failQueued(transferIds, "No se ha subido");
    }
  } finally {
    state.photos.uploading = false;
    uploadButton.textContent = uploadLabel;
    if (!$("#photos-modal").classList.contains("hidden")) updatePhotosSelection();
  }
}

/** One file: ask where to save it. Several: ask for a folder; nothing in it is replaced. */
async function downloadFiles(files: LogicalFile[]) {
  if (!files.length) return;
  let jobs: { file: LogicalFile; dest: string; overwrite: boolean }[];
  if (files.length === 1) {
    const dest = await save({ title: "Guardar archivo", defaultPath: files[0].name });
    if (!dest) return;
    jobs = [{ file: files[0], dest, overwrite: true }];
  } else {
    const dir = await open({
      directory: true,
      title: `Elige dónde guardar ${files.length} archivos`,
    });
    if (!dir || Array.isArray(dir)) return;
    jobs = await Promise.all(
      files.map(async (file) => ({ file, dest: await join(dir, file.name), overwrite: false })),
    );
  }

  const transferIds = jobs.map((job) => queueTransfer("download", job.file.name, job.file.size));
  renderQueue();
  openTransfers();
  const folderId = currentCrumb().id;
  let failed = 0;
  for (const [index, job] of jobs.entries()) {
    try {
      await invoke("download_entry", {
        transferId: transferIds[index],
        id: job.file.normal_id ?? 0,
        name: job.file.name,
        dest: job.dest,
        chunked: job.file.chunked,
        manifestId: job.file.manifest_id,
        parts: job.file.parts,
        size: job.file.size,
        folderId,
        overwrite: job.overwrite,
      });
    } catch (error) {
      failed++;
      if (jobs.length === 1 || isSessionError(String(error))) {
        reportError(error);
        if (jobs.length > 1) break;
      }
    }
  }
  failQueued(transferIds, "Cancelada");
  if (jobs.length === 1) {
    if (!failed) toast(`Descarga completada: ${jobs[0].file.name}`, "success");
  } else if (failed) {
    toast(
      `Descargados ${jobs.length - failed} de ${jobs.length}. Los errores están en Transferencias.`,
      "error",
      10000,
    );
  } else {
    toast(`Descargados ${jobs.length} archivos`, "success");
  }
}

async function trashEntries(entries: (FolderEntry | FileEntry)[]) {
  if (!entries.length) return;
  const files = entries.filter((entry): entry is FileEntry => entry.kind === "file");
  const folders = entries.filter((entry): entry is FolderEntry => entry.kind === "folder");
  const what = entries.length === 1 ? `«${entries[0].name}»` : `${entries.length} elementos`;
  const note = folders.length
    ? folders.length === 1
      ? " La carpeta se mueve con todo su contenido."
      : " Las carpetas se mueven con todo su contenido."
    : "";
  const ok = await confirm(`¿Mover ${what} a la papelera de O2?${note}`, {
    title: "Mover a la papelera",
    kind: "warning",
  });
  if (!ok) return;

  const crumb = currentCrumb();
  const view = crumbKey(crumb);
  const keys = new Set(entries.map((entry) => entry.key));
  let deleted = false;

  // Each CLI delete lists the whole account first, so hiding the rows now
  // is the difference between "nothing happens" and an obvious action.
  rememberRemovals(view, keys);
  if (crumb.virtual) rememberRemovals(crumbKey(ROOT), keys);
  for (const entry of entries) state.trashing.add(entry.key);
  removeEntriesFromView(view, keys);
  state.selected.clear();
  const operation = queueOperation("delete", what, "Eliminando…", true);

  try {
    await invoke("delete_entries", {
      // Field names inside the structs stay as in Rust.
      files: files.map(({ file }) =>
        file.chunked
          ? { ids: file.parts.map((part) => part.id), manifest_id: file.manifest_id }
          : { ids: file.normal_id != null ? [file.normal_id] : [], manifest_id: null },
      ),
      folders: folders.map(({ folder }) => folder.id),
    });
    deleted = true;
    finishOperation(
      operation,
      entries.length === 1 ? "Movido a la papelera" : "Movidos a la papelera",
    );
    toast(
      entries.length === 1 ? `${what} movido a la papelera` : `${what} movidos a la papelera`,
      "success",
    );
  } catch (error) {
    finishOperation(operation, String(error), true);
    reportError(error);
    forgetRemovals(view, keys);
    if (crumb.virtual) forgetRemovals(crumbKey(ROOT), keys);
  } finally {
    for (const entry of entries) state.trashing.delete(entry.key);
    state.selected.clear();
    if (!deleted) {
      // O2 rejected the delete (or only part of it): restore the real rows.
      void refresh({ fresh: true });
    }
  }
}

function createFolder() {
  // Virtual views create in the root, like their uploads.
  const where = currentCrumb().virtual ? [ROOT] : state.stack;
  $("#folder-where").textContent = `En ${where.map((crumb) => crumb.name).join(" › ")}`;
  const input = $("#folder-name") as HTMLInputElement;
  input.value = "";
  $("#folder-error").textContent = "";
  $("#folder-modal").classList.remove("hidden");
  input.focus();
}

function hideFolderModal() {
  $("#folder-modal").classList.add("hidden");
}

async function submitFolder(event: SubmitEvent) {
  event.preventDefault();
  const crumb = currentCrumb();
  const name = ($("#folder-name") as HTMLInputElement).value.trim();
  const error = $("#folder-error");
  if (!name) {
    error.textContent = "Escribe un nombre.";
    return;
  }
  if (name.includes("/")) {
    error.textContent = "El nombre no puede llevar «/».";
    return;
  }
  const here = crumb.virtual ? state.rootFolders : (state.listing?.folders ?? []);
  if (here.some((folder) => folder.name === name)) {
    error.textContent = "Ya hay una carpeta con ese nombre aquí.";
    return;
  }

  const button = $("#folder-create") as HTMLButtonElement;
  button.disabled = true;
  button.textContent = "Creando…";
  error.textContent = "";
  try {
    const created = await invoke<CreatedFolder>("create_folder", {
      parentId: crumb.id,
      parentPath: crumb.path,
      name,
    });
    const folder = created.folder;
    if (created.created) {
      knownEmptyFolders.set(folder.id, Date.now() + EMPTY_FOLDER_GRACE_MS);
    }
    hideFolderModal();
    toast(`Carpeta «${name}» creada`, "success");
    // Virtual views create at the root, so that is the view to remember.
    const view = crumb.virtual ? crumbKey(ROOT) : crumbKey(crumb);
    rememberFolderAdd(view, folder);
    // A new folder only changes this view. Inserting it avoids the full
    // `ls` (up to a minute on a large account) the old refresh triggered.
    if (crumb.virtual) {
      if (!state.rootFolders.some((item) => item.id === folder.id)) {
        state.rootFolders.push(folder);
      }
    } else if (listingView === view && state.listing) {
      if (!state.listing.folders.some((item) => item.id === folder.id)) {
        state.listing.folders.push(folder);
      }
      if (state.stack.length === 1 && !state.rootFolders.some((item) => item.id === folder.id)) {
        state.rootFolders.push(folder);
      }
    } else if (!state.listing && crumbKey(currentCrumb()) === view) {
      // The first listing is still loading (it can take a minute on a large
      // account). Show the folder that was just created instead of a spinner.
      state.listing = {
        path: crumb.path,
        folders: [folder],
        files: [],
        warnings: [],
        virtual_folders: [],
      };
      if (state.stack.length === 1 && !state.rootFolders.some((item) => item.id === folder.id)) {
        state.rootFolders.push(folder);
      }
    }
    // The table keeps its own sorted view of the listing; invalidate it so
    // the folder appears now, not after the next refresh.
    entriesCache.listing = null;
    renderPlaces();
    renderListing();
  } catch (err) {
    error.textContent = String(err);
  } finally {
    button.disabled = false;
    button.textContent = "Crear";
  }
}

async function startLogin() {
  $("#login-btn").setAttribute("disabled", "true");
  setLoginStatus("Abriendo la ventana de login de O2…");
  try {
    const status = await invoke<SessionStatus>("login");
    state.session = status;
    renderSession();
    hideLoginModal();
    toast("Sesión iniciada", "success");
    await refresh({ fresh: true });
  } catch (error) {
    setLoginStatus(String(error), "error");
  } finally {
    $("#login-btn").removeAttribute("disabled");
  }
}

// ---------- Settings ----------

function populateSettings() {
  ($("#chunk-size") as HTMLInputElement).value = String(state.settings.chunk_size_mb);
  ($("#group-camera") as HTMLInputElement).checked = state.settings.group_camera;

  const cliDetails = $("#settings-cli-info");
  cliDetails.replaceChildren();
  if (state.cli?.available) {
    const lines = [
      `Versión: ${state.cli.version}`,
      `Origen: ${state.cli.source}`,
      `Ruta: ${state.cli.path}`,
    ];
    for (const line of lines) {
      const div = document.createElement("div");
      div.textContent = line;
      cliDetails.appendChild(div);
    }
  } else {
    cliDetails.textContent =
      "CLI no instalado. Pulsa «Buscar actualización» para descargar la última versión.";
  }

  const libraryDetails = $("#settings-library");
  libraryDetails.replaceChildren();
  const stats = state.libraryStats;
  const libraryLines = stats
    ? [
        `${plural(stats.files, "archivo", "archivos")} · ${formatSize(stats.bytes)}`,
        `${plural(stats.copies, "copia", "copias")} en O2 · ${plural(stats.devices, "dispositivo", "dispositivos")}`,
        `Última sincronización: ${formatAgo(stats.last_sync)}`,
        stats.pending_snapshot ? "Hay cambios pendientes de subir a O2." : "",
      ].filter(Boolean)
    : ["Sin sincronizar todavía."];
  for (const line of libraryLines) {
    const div = document.createElement("div");
    div.textContent = line;
    libraryDetails.appendChild(div);
  }

  const sessionDetails = $("#settings-session");
  sessionDetails.replaceChildren();
  if (state.session?.logged_in) {
    const lines = [
      "Sesión activa",
      state.session.phone ? `Teléfono: ${state.session.phone}` : "",
      state.session.storage ? `Almacenamiento: ${state.session.storage}` : "",
    ].filter(Boolean);
    for (const line of lines) {
      const div = document.createElement("div");
      div.textContent = line;
      sessionDetails.appendChild(div);
    }
  } else {
    sessionDetails.textContent = "No hay sesión iniciada.";
  }
}

async function saveSettings() {
  const chunk = Number(($("#chunk-size") as HTMLInputElement).value);
  const groupCamera = ($("#group-camera") as HTMLInputElement).checked;
  try {
    state.settings = await invoke<Settings>("save_settings", {
      settings: {
        chunk_size_mb: Number.isFinite(chunk) ? chunk : 3500,
        group_camera: groupCamera,
      },
    });
    toast("Ajustes guardados", "success");
    // The root view changes as soon as grouping is toggled. If we are inside
    // a virtual view and grouping is turned off, go back to the root.
    if (currentCrumb().virtual && !state.settings.group_camera) {
      navigate([ROOT]);
    } else if (state.stack.length === 1) {
      await refresh();
    } else {
      renderPlaces();
    }
  } catch (error) {
    toast(String(error), "error");
  }
}

async function syncUploadLibrary(reconcile: boolean) {
  const syncButton = $("#settings-library-sync") as HTMLButtonElement;
  const reconcileButton = $("#settings-library-reconcile") as HTMLButtonElement;
  const forgetButton = $("#settings-library-forget") as HTMLButtonElement;
  syncButton.disabled = true;
  reconcileButton.disabled = true;
  forgetButton.disabled = true;
  const details = $("#settings-library");
  details.textContent = reconcile ? "Reconciliando con O2…" : "Sincronizando biblioteca…";
  const operation = queueOperation(
    "library",
    reconcile ? "Reconciliar biblioteca" : "Sincronizar biblioteca",
    reconcile ? "Reconciliando con O2…" : "Sincronizando biblioteca…",
  );
  try {
    const stats = reconcile
      ? await invoke<LibraryStats>("library_reconcile")
      : await invoke<LibraryStats>("library_sync", { force: true });
    state.libraryStats = stats;
    finishOperation(operation, "Biblioteca actualizada");
    toast(
      reconcile
        ? "Biblioteca reconciliada con O2"
        : `Biblioteca sincronizada: ${plural(stats.files, "archivo", "archivos")}`,
      "success",
    );
  } catch (error) {
    finishOperation(operation, String(error), true);
    toast(String(error), "error", 10000);
  } finally {
    syncButton.disabled = false;
    reconcileButton.disabled = false;
    forgetButton.disabled = false;
    populateSettings();
  }
}

async function forgetUploadLibrary() {
  const syncButton = $("#settings-library-sync") as HTMLButtonElement;
  const reconcileButton = $("#settings-library-reconcile") as HTMLButtonElement;
  const forgetButton = $("#settings-library-forget") as HTMLButtonElement;
  const ok = await confirm(
    "Se borrarán los hashes y los snapshots de este equipo en O2. Los snapshots de otros equipos se conservarán y se volverán a sincronizar. ¿Olvidar la biblioteca de este equipo?",
    {
      title: "Olvidar biblioteca",
      kind: "warning",
    },
  );
  if (!ok) return;

  syncButton.disabled = true;
  reconcileButton.disabled = true;
  forgetButton.disabled = true;
  const details = $("#settings-library");
  details.textContent = "Olvidando la biblioteca de este equipo…";
  const operation = queueOperation(
    "library",
    "Olvidar biblioteca",
    "Borrando snapshots de este equipo…",
  );
  try {
    const stats = await invoke<LibraryStats>("library_forget");
    state.libraryStats = stats;
    finishOperation(operation, "Biblioteca olvidada");
    toast("Biblioteca olvidada en este equipo", "success");
  } catch (error) {
    finishOperation(operation, String(error), true);
    toast(String(error), "error", 10000);
  } finally {
    syncButton.disabled = false;
    reconcileButton.disabled = false;
    forgetButton.disabled = false;
    populateSettings();
  }
}

async function checkCliUpdate() {
  const result = $("#cli-update-result");
  result.textContent = "Consultando GitHub…";
  $("#settings-install-update").classList.add("hidden");
  try {
    const release = await invoke<CliRelease>("check_cli_update");
    state.pendingRelease = release;
    if (release.update_available && release.latest && release.asset_url) {
      result.textContent = `Hay una versión nueva: ${release.latest} (actual: ${release.current}).`;
      $("#settings-install-update").classList.remove("hidden");
    } else if (release.latest) {
      result.textContent = `Estás al día (${release.current}). Última publicada: ${release.latest}.`;
    } else {
      result.textContent = "No se pudo determinar la última versión.";
    }
  } catch (error) {
    result.textContent = String(error);
  }
}

async function installCliUpdate() {
  const release = state.pendingRelease;
  if (!release?.latest || !release.asset_url) return;
  const result = $("#cli-update-result");
  result.textContent = `Descargando ${release.latest}…`;
  $("#settings-install-update").setAttribute("disabled", "true");
  const operation = queueOperation(
    "cli",
    `Actualizar CLI a ${release.latest}`,
    `Descargando ${release.latest}…`,
  );
  try {
    const info = await invoke<CliInfo>("install_cli_update", {
      version: release.latest,
      url: release.asset_url,
    });
    state.cli = info;
    populateSettings();
    result.textContent = `CLI actualizado a ${info.version}.`;
    finishOperation(operation, "CLI actualizado");
    toast("CLI actualizado", "success");
    if (!state.session?.logged_in) {
      hideSettings();
      showLoginModal();
      setLoginStatus("CLI instalado. Inicia sesión para continuar.");
    }
  } catch (error) {
    finishOperation(operation, String(error), true);
    result.textContent = String(error);
    toast(String(error), "error", 10000);
  } finally {
    $("#settings-install-update").removeAttribute("disabled");
  }
}

async function logout() {
  try {
    await invoke("logout");
    state.session = {
      logged_in: false,
      raw: "",
      phone: null,
      storage: null,
    };
    renderSession();
    populateSettings();
    toast("Sesión cerrada", "success");
    showLoginModal();
  } catch (error) {
    toast(String(error), "error");
  }
}

// ---------- Keyboard ----------

function isTyping(target: EventTarget | null): boolean {
  return (
    (target instanceof HTMLInputElement && target.type !== "checkbox") ||
    target instanceof HTMLTextAreaElement
  );
}

function onKeyDown(event: KeyboardEvent) {
  const mod = event.metaKey || event.ctrlKey;
  const key = event.key;
  const search = $("#search") as HTMLInputElement;

  if (key === "Escape") {
    if (menuOpen()) hideMenu();
    else if (transfersOpen()) closeTransfers();
    else if (anyModalOpen()) {
      if (conflictResolve) {
        // Cancel the duplicate prompt but leave the Photos view open.
        finishLibraryConflict("cancel");
      } else {
        hideSettings();
        hideFolderModal();
        hidePhotos();
        if (state.session?.logged_in) hideLoginModal();
      }
    } else if (event.target === search && search.value) {
      search.value = "";
      state.filter = "";
      renderListing();
    } else if (event.target === search) search.blur();
    else if (state.selected.size) clearSelection();
    return;
  }
  if (anyModalOpen()) return;
  if (menuOpen() && (key === "ArrowDown" || key === "ArrowUp")) {
    event.preventDefault();
    moveMenuFocus(key === "ArrowDown" ? 1 : -1);
    return;
  }
  if (mod && key.toLowerCase() === "r") {
    event.preventDefault();
    void refresh({ fresh: true });
    return;
  }
  if (mod && key.toLowerCase() === "f") {
    event.preventDefault();
    search.focus();
    search.select();
    return;
  }
  if (isTyping(event.target)) return;

  if (mod && event.shiftKey && key.toLowerCase() === "n") {
    event.preventDefault();
    createFolder();
  } else if (mod && key.toLowerCase() === "a") {
    event.preventDefault();
    selectAll();
  } else if (mod && (key === "ArrowUp" || key === "[")) {
    event.preventDefault();
    goUp();
  } else if (key === "Enter" || (mod && key === "ArrowDown")) {
    event.preventDefault();
    openSelection();
  } else if (key === "Delete" || (mod && key === "Backspace")) {
    event.preventDefault();
    void trashEntries(selectedEntries());
  } else if (!mod && (key === "ArrowDown" || key === "ArrowUp")) {
    event.preventDefault();
    moveSelection(key === "ArrowDown" ? 1 : -1, event.shiftKey);
  }
}

// ---------- Start ----------

async function bootstrap() {
  renderPathbar();
  renderPlaces();
  renderQueue();

  try {
    state.cli = await invoke<CliInfo>("get_cli_info");
  } catch (error) {
    toast(String(error), "error", 10000);
  }

  try {
    state.settings = await invoke<Settings>("get_settings");
    renderPlaces();
  } catch {
    // defaults
  }

  try {
    const access = await invoke<PhotosAccess>("photos_access_status");
    state.photos.available = access.available;
    state.photos.status = access.status;
  } catch {
    // Not macOS, or an older build without the Photos bridge.
    state.photos.available = false;
  }

  try {
    state.libraryStats = await invoke<LibraryStats>("library_status");
  } catch {
    // The library is optional; uploads still work without it.
  }

  await listen<TransferProgress>("transfer-progress", (event) => {
    const item = event.payload;
    trackRate(item);
    state.queue.set(item.id, item);
    forgetOldTransfers();
    renderQueue();
  });

  await listen<{ stream: string; line: string }>("cli-log", (event) => {
    console.log(`[o2cloud:${event.payload.stream}] ${event.payload.line}`);
  });

  const webview = getCurrentWebview();
  await webview.onDragDropEvent((event) => {
    const payload = event.payload;
    const overlay = $("#drop-overlay");
    if (payload.type === "enter" || payload.type === "over") {
      overlay.classList.remove("hidden");
    } else if (payload.type === "drop") {
      overlay.classList.add("hidden");
      void prepareUploadPaths(payload.paths);
    } else {
      overlay.classList.add("hidden");
    }
  });

  // Toolbar and list
  $("#back-btn").addEventListener("click", goUp);
  $("#refresh-btn").addEventListener("click", () => void refresh({ fresh: true }));
  $("#search").addEventListener("input", (event) => {
    state.filter = (event.target as HTMLInputElement).value;
    state.visibleLimit = PAGE;
    state.selected.clear();
    renderListing();
  });
  $("#upload-btn").addEventListener("click", (event) => {
    event.stopPropagation();
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuEntry[] = [
      { label: "Archivos…", run: () => void chooseFiles(false) },
      { label: "Carpeta…", run: () => void chooseFiles(true) },
    ];
    if (state.photos.available) {
      items.push("separator", { label: "Desde Fotos…", run: () => void openPhotos() });
    }
    showMenu(items, rect.left, rect.bottom + 4);
  });
  $("#new-folder-btn").addEventListener("click", createFolder);
  $("#selection-download").addEventListener("click", downloadSelected);
  $("#selection-trash").addEventListener("click", () => void trashEntries(selectedEntries()));
  $("#selection-clear").addEventListener("click", clearSelection);
  $("#select-all").addEventListener("change", (event) => {
    if ((event.target as HTMLInputElement).checked) selectAll();
    else clearSelection();
  });
  for (const button of document.querySelectorAll<HTMLButtonElement>(".sort")) {
    button.addEventListener("click", () => {
      const key = button.dataset.sort as SortKey;
      // Names A→Z first; sizes and dates largest/newest first.
      state.sort =
        state.sort.key === key
          ? { key, dir: state.sort.dir === 1 ? -1 : 1 }
          : { key, dir: key === "name" ? 1 : -1 };
      saveSort();
      renderListing();
    });
  }
  $("#load-more").addEventListener("click", () => {
    state.visibleLimit += PAGE;
    renderListing();
  });
  const tableWrap = $("#table-wrap");
  tableWrap.addEventListener("click", (event) => {
    if (!(event.target as HTMLElement).closest("tr")) clearSelection();
  });
  tableWrap.addEventListener("contextmenu", (event) => {
    if ((event.target as HTMLElement).closest("thead")) return;
    event.preventDefault();
    clearSelection();
    showMenu(backgroundMenu(), event.clientX, event.clientY);
  });
  tableWrap.addEventListener("scroll", hideMenu, { passive: true });

  // Top bar
  $("#transfers-btn").addEventListener("click", (event) => {
    event.stopPropagation();
    if (transfersOpen()) closeTransfers();
    else openTransfers();
  });
  $("#transfers-clear").addEventListener("click", clearFinishedTransfers);
  $("#settings-btn").addEventListener("click", showSettings);
  $("#session-badge").addEventListener("click", () => {
    if (state.session?.logged_in) showSettings();
    else showLoginModal();
  });

  // Modals
  $("#settings-close").addEventListener("click", hideSettings);
  $("#login-btn").addEventListener("click", () => void startLogin());
  $("#login-cancel").addEventListener("click", hideLoginModal);
  $("#folder-form").addEventListener("submit", (event) => void submitFolder(event as SubmitEvent));
  $("#folder-cancel").addEventListener("click", hideFolderModal);
  $("#save-settings").addEventListener("click", () => void saveSettings());
  $("#settings-library-sync").addEventListener("click", () => void syncUploadLibrary(false));
  $("#settings-library-reconcile").addEventListener("click", () =>
    void syncUploadLibrary(true),
  );
  $("#settings-library-forget").addEventListener("click", () => void forgetUploadLibrary());
  $("#settings-check-update").addEventListener("click", () => void checkCliUpdate());
  $("#settings-install-update").addEventListener("click", () => void installCliUpdate());
  $("#settings-login").addEventListener("click", () => {
    hideSettings();
    showLoginModal();
  });
  $("#settings-logout").addEventListener("click", () => void logout());

  // Photos
  $("#photos-close").addEventListener("click", hidePhotos);
  $("#photos-cancel").addEventListener("click", hidePhotos);
  $("#photos-grant").addEventListener("click", () => void handlePhotosPermission());
  $("#photos-search").addEventListener("input", (event) => {
    state.photos.filter = (event.target as HTMLInputElement).value;
    state.photos.visibleLimit = PHOTO_PAGE;
    renderPhotos();
  });
  $("#photos-media").addEventListener("change", (event) => {
    state.photos.mediaType = (event.target as HTMLSelectElement).value;
    state.photos.selected.clear();
    state.photos.visibleLimit = PHOTO_PAGE;
    void loadPhotos();
  });
  $("#photos-only-large").addEventListener("change", (event) => {
    state.photos.onlyLarge = (event.target as HTMLInputElement).checked;
    state.photos.visibleLimit = PHOTO_PAGE;
    renderPhotos();
  });
  $("#photos-hide-existing").addEventListener("change", (event) => {
    state.photos.hideExisting = (event.target as HTMLInputElement).checked;
    state.photos.visibleLimit = PHOTO_PAGE;
    renderPhotos();
  });
  $("#photos-select-pending").addEventListener("click", selectPendingPhotos);
  $("#photos-select-all").addEventListener("click", selectAllPhotos);
  $("#photos-select-none").addEventListener("click", clearPhotoSelection);
  $("#photos-more").addEventListener("click", () => {
    state.photos.visibleLimit += PHOTO_PAGE;
    renderPhotos();
  });
  $("#photos-upload").addEventListener("click", photosUploadSelected);

  // Upload-library conflicts
  $("#library-conflict-cancel").addEventListener("click", () => finishLibraryConflict("cancel"));
  $("#library-conflict-skip").addEventListener("click", () => setAllConflictChoices(false));
  $("#library-conflict-upload").addEventListener("click", () => setAllConflictChoices(true));
  $("#library-conflict-continue").addEventListener("click", () =>
    finishLibraryConflict("continue"),
  );

  // Close menus and the transfers popover when clicking elsewhere.
  document.addEventListener("mousedown", (event) => {
    const target = event.target as HTMLElement;
    if (menuOpen() && !target.closest("#menu")) hideMenu();
    if (transfersOpen() && !target.closest("#transfers-popover, #transfers-btn")) closeTransfers();
  });
  window.addEventListener("blur", hideMenu);
  window.addEventListener("resize", () => {
    hideMenu();
    if (transfersOpen()) openTransfers();
  });
  // No browser menu ("Reload", "Inspect"); ours is shown where it applies.
  window.addEventListener("contextmenu", (event) => {
    if (!isTyping(event.target)) event.preventDefault();
  });
  window.addEventListener("keydown", onKeyDown);

  // Only now load the account: a listing can take ~50 s on a large account,
  // and the window must stay usable (creating a folder, opening Photos,…)
  // while it runs.
  if (state.cli?.available) {
    await refreshSession();
    // Listing without a session would open the CLI's login window on top of
    // ours; the listing loads once the login finishes.
    if (state.session?.logged_in) await refresh({ fresh: true });
  } else {
    showSettings();
    toast("Instala el CLI de O2 Cloud desde Ajustes para empezar.", "info", 9000);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  void bootstrap();
});
