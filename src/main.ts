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
const SORT_STORAGE = "o2cloud.sort";

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

function toast(message: string, type: "info" | "success" | "error" = "info", timeout = 5000) {
  const container = $("#toast-container");
  const el = document.createElement("div");
  el.className = `toast ${type}`;
  el.textContent = message;
  container.appendChild(el);
  window.setTimeout(() => {
    el.classList.add("leaving");
    window.setTimeout(() => el.remove(), 250);
  }, timeout);
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
  return [
    { label: "Subir archivos…", run: () => void chooseFiles(false) },
    { label: "Subir carpeta…", run: () => void chooseFiles(true) },
    { label: "Nueva carpeta…", hint: "⇧⌘N", run: () => createFolder() },
    "separator",
    { label: "Recargar", hint: "⌘R", run: () => void refresh({ fresh: true }) },
  ];
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
  view.name.textContent = `${item.kind === "download" ? "↓" : "↑"} ${item.name}`;
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

/** The toolbar button: how many transfers run, and how far they are overall. */
function renderTransfersButton() {
  const active = [...state.queue.values()].filter((item) => !isFinished(item));
  const count = $("#transfers-count");
  count.textContent = String(active.length);
  count.classList.toggle("hidden", !active.length);
  $("#transfers-btn").classList.toggle("active", active.length > 0);

  const measured = active.filter((item) => item.bytes_total > 0);
  const total = measured.reduce((sum, item) => sum + item.bytes_total, 0);
  const done = measured.reduce((sum, item) => sum + Math.min(item.bytes_done, item.bytes_total), 0);
  $("#transfers-progress").classList.toggle("hidden", !total);
  if (total) $("#transfers-progress-fill").style.width = `${Math.round((done / total) * 100)}%`;
}

function renderQueue() {
  const list = $("#queue-list");
  const items = [...state.queue.values()].slice(-50);

  const shown = new Set(items.map((item) => item.id));
  for (const [id, view] of queueRows) {
    if (!shown.has(id)) {
      view.row.remove();
      queueRows.delete(id);
    }
  }
  list.querySelector(".queue-empty")?.remove();
  ($("#transfers-clear") as HTMLButtonElement).disabled = !items.some(isFinished);
  renderTransfersButton();

  if (!items.length) {
    const empty = document.createElement("div");
    empty.className = "queue-empty";
    empty.textContent = "No hay transferencias.";
    list.appendChild(empty);
    return;
  }

  for (const item of items) {
    let view = queueRows.get(item.id);
    if (!view) {
      view = createQueueRow();
      queueRows.set(item.id, view);
      list.appendChild(view.row);
    }
    updateQueueRow(view, item);
  }
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

/** List the current folder. `fresh` skips the backend's listing cache. */
async function refresh(options: { fresh?: boolean } = {}) {
  const seq = ++refreshSeq;
  const crumb = currentCrumb();
  const view = crumbKey(crumb);
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
    state.listing = listing;
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

async function refreshSession() {
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
  if (!state.session.logged_in) {
    showLoginModal();
    setLoginStatus("Inicia sesión para acceder a tus archivos.");
  }
}

// ---------- Actions ----------

async function uploadPaths(paths: string[]) {
  if (!paths.length) return;
  if (!state.session?.logged_in) {
    showLoginModal();
    return;
  }
  const crumb = currentCrumb();
  const view = crumbKey(crumb);
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
    });
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

async function chooseFiles(folder: boolean) {
  const selected = await open({
    multiple: true,
    directory: folder,
    title: folder ? "Elige carpetas para subir" : "Elige archivos para subir",
  });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  await uploadPaths(paths);
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

  for (const entry of entries) state.trashing.add(entry.key);
  renderListing();
  toast(`Moviendo ${what} a la papelera…`);
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
    toast(
      entries.length === 1 ? `${what} movido a la papelera` : `${what} movidos a la papelera`,
      "success",
    );
  } catch (error) {
    reportError(error);
  } finally {
    for (const entry of entries) state.trashing.delete(entry.key);
    state.selected.clear();
    await refresh({ fresh: true });
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
  error.textContent = "";
  try {
    await invoke("create_folder", { parentPath: crumb.path, name });
    hideFolderModal();
    toast(`Carpeta «${name}» creada`, "success");
    await refresh({ fresh: true });
  } catch (err) {
    error.textContent = String(err);
  } finally {
    button.disabled = false;
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
  try {
    const info = await invoke<CliInfo>("install_cli_update", {
      version: release.latest,
      url: release.asset_url,
    });
    state.cli = info;
    populateSettings();
    result.textContent = `CLI actualizado a ${info.version}.`;
    toast("CLI actualizado", "success");
    if (!state.session?.logged_in) {
      hideSettings();
      showLoginModal();
      setLoginStatus("CLI instalado. Inicia sesión para continuar.");
    }
  } catch (error) {
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
      hideSettings();
      hideFolderModal();
      if (state.session?.logged_in) hideLoginModal();
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

  if (state.cli?.available) {
    await refreshSession();
    await refresh({ fresh: true });
  } else {
    showSettings();
    toast("Instala el CLI de O2 Cloud desde Ajustes para empezar.", "info", 9000);
  }

  await listen<TransferProgress>("transfer-progress", (event) => {
    const item = event.payload;
    trackRate(item);
    state.queue.set(item.id, item);
    // Keep the map bounded during long folder uploads.
    if (state.queue.size > 200) {
      const first = state.queue.keys().next().value;
      if (first) {
        state.queue.delete(first);
        transferRates.delete(first);
      }
    }
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
      void uploadPaths(payload.paths);
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
    showMenu(
      [
        { label: "Archivos…", run: () => void chooseFiles(false) },
        { label: "Carpeta…", run: () => void chooseFiles(true) },
      ],
      rect.left,
      rect.bottom + 4,
    );
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
  $("#settings-check-update").addEventListener("click", () => void checkCliUpdate());
  $("#settings-install-update").addEventListener("click", () => void installCliUpdate());
  $("#settings-login").addEventListener("click", () => {
    hideSettings();
    showLoginModal();
  });
  $("#settings-logout").addEventListener("click", () => void logout());

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
}

window.addEventListener("DOMContentLoaded", () => {
  void bootstrap();
});
