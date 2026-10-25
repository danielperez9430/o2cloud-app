import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

const state = {
  stack: [{ id: null, name: "O2 Cloud", path: "/" }] as Crumb[],
  listing: null as LogicalListing | null,
  cli: null as CliInfo | null,
  session: null as SessionStatus | null,
  settings: { chunk_size_mb: 3500, group_camera: true } as Settings,
  queue: new Map<string, TransferProgress>(),
  uploadsInFlight: 0,
  pendingRelease: null as CliRelease | null,
  loading: false,
  visibleLimit: 500,
};

const $ = <T extends HTMLElement>(selector: string): T => {
  const el = document.querySelector<T>(selector);
  if (!el) throw new Error(`No existe ${selector}`);
  return el;
};

function currentCrumb(): Crumb {
  return state.stack[state.stack.length - 1];
}

function joinPath(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent.replace(/\/+$/, "")}/${name}`;
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

function formatDate(value: string | number | null): string {
  if (value == null || value === "") return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  try {
    return date.toLocaleString("es-ES", {
      day: "2-digit",
      month: "2-digit",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "—";
  }
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

/** Show an error; if it says the O2 session is gone, ask for a new login. */
function reportError(error: unknown, timeout = 10000) {
  const message = String(error);
  toast(message, "error", timeout);
  if (/sesión de O2 ha caducado|Authentication failed|Not logged in/i.test(message)) {
    void refreshSession();
  }
}

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

function renderCli() {
  const el = $("#cli-info");
  if (state.cli?.available) {
    el.textContent = `v${state.cli.version} · ${state.cli.source}`;
    el.title = state.cli.path;
  } else {
    el.textContent = "No instalado";
    el.title = "Instálalo desde Ajustes";
  }
}

function renderBreadcrumbs() {
  const nav = $("#breadcrumbs");
  nav.replaceChildren();
  state.stack.forEach((crumb, index) => {
    if (index > 0) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "/";
      nav.appendChild(sep);
    }
    const button = document.createElement("button");
    button.className = "crumb";
    button.textContent = crumb.name;
    if (index === state.stack.length - 1) button.classList.add("current");
    button.addEventListener("click", () => {
      state.stack = state.stack.slice(0, index + 1);
      void refresh();
    });
    nav.appendChild(button);
  });
}

function renderWarnings() {
  const box = $("#warnings");
  const warnings = state.listing?.warnings ?? [];
  if (!warnings.length) {
    box.classList.add("hidden");
    box.replaceChildren();
    return;
  }
  box.classList.remove("hidden");
  box.replaceChildren();
  const title = document.createElement("strong");
  title.textContent = "Avisos";
  box.appendChild(title);
  const ul = document.createElement("ul");
  for (const warning of warnings) {
    const li = document.createElement("li");
    li.textContent = warning;
    ul.appendChild(li);
  }
  box.appendChild(ul);
}

function makeIcon(kind: "folder" | "file" | "chunk"): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = `row-icon ${kind}`;
  span.textContent = kind === "folder" ? "📁" : kind === "chunk" ? "🧩" : "📄";
  return span;
}

function makeAction(label: string, title: string, onClick: () => void, danger = false): HTMLButtonElement {
  const button = document.createElement("button");
  button.className = `row-action${danger ? " danger" : ""}`;
  button.textContent = label;
  button.title = title;
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

function renderListing() {
  const listing = state.listing;
  if (!listing) return;
  const crumb = currentCrumb();

  $("#folder-title").textContent = crumb.name === "O2 Cloud" ? "O2 Cloud" : crumb.name;
  const folderCount = listing.folders.length;
  const fileCount = listing.files.length;
  const virtualFolders = listing.virtual_folders ?? [];
  const visibleFolders = listing.folders.slice(0, state.visibleLimit);
  const remaining = Math.max(0, state.visibleLimit - visibleFolders.length);
  const visibleFiles = listing.files.slice(0, remaining);
  const shown = visibleFolders.length + visibleFiles.length;
  const virtualLabel = virtualFolders.length
    ? `${virtualFolders.length} carpeta${virtualFolders.length === 1 ? "" : "s"} virtual${
        virtualFolders.length === 1 ? "" : "es"
      } · `
    : "";
  let context = "";
  if (crumb.virtual === "camera") {
    context = " · elige un año (vista virtual)";
  } else if (crumb.virtual === "camera/all") {
    context = " · todas las capturas (vista virtual)";
  } else if (crumb.virtual?.startsWith("camera/")) {
    context = ` · capturas de ${crumb.name} (vista virtual)`;
  }
  $("#folder-subtitle").textContent = state.loading
    ? "Cargando…"
    : `${virtualLabel}${folderCount} carpeta${folderCount === 1 ? "" : "s"} · ${fileCount} archivo${
        fileCount === 1 ? "" : "s"
      }${shown < folderCount + fileCount ? ` · mostrando ${shown}` : ""}${context}`;

  $("#load-more-wrap").classList.toggle(
    "hidden",
    state.loading || shown >= folderCount + fileCount,
  );

  const tbody = $("#file-list");
  tbody.replaceChildren();

  for (const virtual of virtualFolders) {
    const tr = document.createElement("tr");
    tr.className = "row virtual-row folder-row";
    tr.addEventListener("dblclick", () => openVirtualFolder(virtual));

    const nameCell = document.createElement("td");
    nameCell.className = "col-name";
    const nameWrap = document.createElement("div");
    nameWrap.className = "name-wrap";
    const icon = document.createElement("span");
    icon.className = "row-icon";
    icon.textContent = virtual.kind === "camera" ? "📷" : virtual.kind === "year" ? "🗓️" : "🗂️";
    nameWrap.appendChild(icon);
    const nameButton = document.createElement("button");
    nameButton.className = "name-button";
    nameButton.textContent = virtual.name;
    nameButton.addEventListener("click", () => openVirtualFolder(virtual));
    nameWrap.appendChild(nameButton);
    const pill = document.createElement("span");
    pill.className = "pill";
    pill.textContent = "virtual";
    nameWrap.appendChild(pill);
    nameCell.appendChild(nameWrap);

    const sizeCell = document.createElement("td");
    sizeCell.className = "col-size";
    sizeCell.textContent = formatSize(virtual.size);

    const dateCell = document.createElement("td");
    dateCell.className = "col-date";
    dateCell.textContent = `${virtual.files} archivo${virtual.files === 1 ? "" : "s"}`;

    const actions = document.createElement("td");
    actions.className = "col-actions";
    actions.appendChild(
      makeAction("Abrir", "Abrir carpeta virtual", () => openVirtualFolder(virtual)),
    );

    tr.append(nameCell, sizeCell, dateCell, actions);
    tbody.appendChild(tr);
  }

  for (const folder of visibleFolders) {
    const tr = document.createElement("tr");
    tr.className = "row folder-row";
    tr.addEventListener("dblclick", () => openFolder(folder));

    const nameCell = document.createElement("td");
    nameCell.className = "col-name";
    const nameWrap = document.createElement("div");
    nameWrap.className = "name-wrap";
    nameWrap.appendChild(makeIcon("folder"));
    const nameButton = document.createElement("button");
    nameButton.className = "name-button";
    nameButton.textContent = folder.name;
    nameButton.addEventListener("click", () => openFolder(folder));
    nameWrap.appendChild(nameButton);
    nameCell.appendChild(nameWrap);

    const sizeCell = document.createElement("td");
    sizeCell.className = "col-size";
    sizeCell.textContent = folder.size ? formatSize(folder.size) : "—";

    const dateCell = document.createElement("td");
    dateCell.className = "col-date";
    dateCell.textContent = "—";

    const actions = document.createElement("td");
    actions.className = "col-actions";
    actions.appendChild(makeAction("Abrir", "Abrir carpeta", () => openFolder(folder)));
    actions.appendChild(
      makeAction("Borrar", "Mover a la papelera", () => void removeFolder(folder), true),
    );

    tr.append(nameCell, sizeCell, dateCell, actions);
    tbody.appendChild(tr);
  }

  for (const file of visibleFiles) {
    const tr = document.createElement("tr");
    tr.className = "row file-row";
    tr.addEventListener("dblclick", () => void downloadFile(file));

    const nameCell = document.createElement("td");
    nameCell.className = "col-name";
    const nameWrap = document.createElement("div");
    nameWrap.className = "name-wrap";
    nameWrap.appendChild(makeIcon(file.chunked ? "chunk" : "file"));
    const nameSpan = document.createElement("span");
    nameSpan.className = "name-text";
    nameSpan.textContent = file.name;
    nameWrap.appendChild(nameSpan);
    if (file.chunked) {
      const badge = document.createElement("span");
      badge.className = `pill${file.incomplete ? " warning" : ""}`;
      badge.textContent = file.incomplete ? "incompleto" : "trozos";
      badge.title = file.incomplete
        ? "Faltan el manifiesto o algún fragmento"
        : "Archivo dividido automáticamente para superar el límite de 4 GB";
      nameWrap.appendChild(badge);
    }
    nameCell.appendChild(nameWrap);

    const sizeCell = document.createElement("td");
    sizeCell.className = "col-size";
    sizeCell.textContent = formatSize(file.size);

    const dateCell = document.createElement("td");
    dateCell.className = "col-date";
    dateCell.textContent = formatDate(file.date);

    const actions = document.createElement("td");
    actions.className = "col-actions";
    actions.appendChild(makeAction("Bajar", "Descargar", () => void downloadFile(file)));
    actions.appendChild(
      makeAction("Borrar", "Mover a la papelera", () => void removeFile(file), true),
    );

    tr.append(nameCell, sizeCell, dateCell, actions);
    tbody.appendChild(tr);
  }

  $("#empty-state").classList.toggle(
    "hidden",
    folderCount + fileCount + virtualFolders.length > 0,
  );
}

function renderQueue() {
  const list = $("#queue-list");
  const items = [...state.queue.values()].slice(-30);
  $("#queue-count").textContent = String(items.filter((item) => item.phase !== "done").length || items.length);
  list.replaceChildren();

  if (!items.length) {
    const empty = document.createElement("div");
    empty.className = "queue-empty";
    empty.textContent = "No hay transferencias.";
    list.appendChild(empty);
    return;
  }

  for (const item of items) {
    const row = document.createElement("div");
    row.className = `queue-item ${item.phase}`;

    const top = document.createElement("div");
    top.className = "queue-item-top";
    const name = document.createElement("span");
    name.className = "queue-name";
    name.textContent = `${item.kind === "download" ? "↓" : "↑"} ${item.name}`;
    const message = document.createElement("span");
    message.className = "queue-message";
    message.textContent = item.message;
    top.append(name, message);

    const bar = document.createElement("div");
    bar.className = "progress";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    const ratio =
      item.bytes_total > 0
        ? Math.min(1, item.bytes_done / item.bytes_total)
        : item.total > 0
          ? Math.min(1, item.current / item.total)
          : item.phase === "done"
            ? 1
            : 0;
    fill.style.width = `${Math.round(ratio * 100)}%`;
    if (item.phase === "done") fill.classList.add("done");
    bar.appendChild(fill);

    row.append(top, bar);
    list.appendChild(row);
  }
}

// A listing can take several seconds; only the latest one may be shown.
let refreshSeq = 0;

/** List the current folder. `fresh` skips the backend's listing cache. */
async function refresh(options: { fresh?: boolean } = {}) {
  const seq = ++refreshSeq;
  const crumb = currentCrumb();
  state.visibleLimit = 500;
  state.loading = true;
  const tableWrap = document.querySelector<HTMLDivElement>(".table-wrap");
  if (tableWrap) tableWrap.scrollTop = 0;
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
    if (listing.path && listing.path !== crumb.path && listing.path !== "/") {
      crumb.path = listing.path;
    }
    renderBreadcrumbs();
    renderWarnings();
    renderListing();
    if (options.fresh) void refreshQuota();
  } catch (error) {
    if (seq !== refreshSeq) return;
    const message = String(error);
    reportError(error, 9000);
    state.listing = {
      path: crumb.path,
      folders: [],
      files: [],
      warnings: [message],
      virtual_folders: [],
    };
    renderWarnings();
    renderListing();
  } finally {
    if (seq === refreshSeq) {
      state.loading = false;
      renderListing();
    }
  }
}

async function refreshQuota() {
  try {
    const raw = await invoke<string>("get_quota");
    const used = raw.match(/Used:\s*([^\n]+)/i)?.[1]?.trim();
    const free = raw.match(/Free:\s*([^\n]+)/i)?.[1]?.trim();
    if (used) {
      $("#quota").textContent = used;
      $("#quota").title = free ? `Libre: ${free}` : raw;
    } else {
      $("#quota").textContent = raw.split("\n")[0] ?? "—";
    }
  } catch {
    $("#quota").textContent = "—";
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

function openFolder(folder: RemoteFolder) {
  const crumb = currentCrumb();
  state.stack.push({
    id: folder.id,
    name: folder.name,
    path: joinPath(crumb.path, folder.name),
  });
  void refresh();
}

function openVirtualFolder(folder: VirtualFolder) {
  const crumb = currentCrumb();
  state.stack.push({
    id: null,
    name: folder.name,
    path: crumb.path,
    virtual: folder.key,
  });
  void refresh();
}

async function uploadPaths(paths: string[]) {
  if (!paths.length) return;
  if (!state.session?.logged_in) {
    showLoginModal();
    return;
  }
  const crumb = currentCrumb();
  const refreshIfStillHere = () => {
    if (currentCrumb().path === crumb.path) void refresh({ fresh: true });
  };
  // The backend runs uploads one at a time, in order.
  const items = `${paths.length} elemento${paths.length === 1 ? "" : "s"}`;
  toast(
    state.uploadsInFlight > 0
      ? `En cola: ${items}. Empezará al terminar la subida en curso.`
      : `Subiendo ${items}…`,
  );
  state.uploadsInFlight++;
  try {
    const summary = await invoke<UploadSummary>("upload_paths", {
      paths,
      folderId: crumb.id,
      folderPath: crumb.path,
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

async function downloadFile(file: LogicalFile) {
  const dest = await save({
    title: "Guardar archivo",
    defaultPath: file.name,
  });
  if (!dest) return;
  toast(`Descargando ${file.name}…`);
  try {
    await invoke("download_entry", {
      id: file.normal_id ?? 0,
      name: file.name,
      dest,
      chunked: file.chunked,
      manifestId: file.manifest_id,
      parts: file.parts,
      folderId: currentCrumb().id,
    });
    toast(`Descarga completada: ${file.name}`, "success");
  } catch (error) {
    reportError(error);
  }
}

async function removeFile(file: LogicalFile) {
  const ok = await confirm(`¿Mover "${file.name}" a la papelera de O2?`, {
    title: "Eliminar archivo",
    kind: "warning",
  });
  if (!ok) return;
  toast(`Moviendo ${file.name} a la papelera…`);
  try {
    if (file.chunked) {
      await invoke("delete_file", {
        ids: file.parts.map((part) => part.id),
        manifestId: file.manifest_id,
      });
    } else if (file.normal_id) {
      await invoke("delete_file", { ids: [file.normal_id], manifestId: null });
    }
    toast(`${file.name} movido a la papelera`, "success");
    await refresh({ fresh: true });
  } catch (error) {
    reportError(error);
  }
}

async function removeFolder(folder: RemoteFolder) {
  const ok = await confirm(`¿Mover la carpeta "${folder.name}" y todo su contenido a la papelera?`, {
    title: "Eliminar carpeta",
    kind: "warning",
  });
  if (!ok) return;
  toast(`Moviendo la carpeta ${folder.name} a la papelera…`);
  try {
    await invoke("delete_folder", { id: folder.id });
    toast(`Carpeta ${folder.name} movida a la papelera`, "success");
    await refresh({ fresh: true });
  } catch (error) {
    reportError(error);
  }
}

async function createFolder() {
  const name = window.prompt("Nombre de la nueva carpeta");
  if (!name?.trim()) return;
  try {
    await invoke("create_folder", {
      parentPath: currentCrumb().path,
      name: name.trim(),
    });
    toast(`Carpeta "${name.trim()}" creada`, "success");
    await refresh({ fresh: true });
  } catch (error) {
    reportError(error);
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

function populateSettings() {
  $("#chunk-size").setAttribute("value", String(state.settings.chunk_size_mb));
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
      state.stack = [{ id: null, name: "O2 Cloud", path: "/" }];
    }
    if (currentCrumb().path === "/" && !currentCrumb().virtual) {
      await refresh();
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
    renderCli();
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

async function bootstrap() {
  renderBreadcrumbs();
  renderQueue();

  try {
    state.cli = await invoke<CliInfo>("get_cli_info");
    renderCli();
  } catch (error) {
    toast(String(error), "error", 10000);
  }

  try {
    state.settings = await invoke<Settings>("get_settings");
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
    state.queue.set(item.id, item);
    // Keep the map bounded during long folder uploads.
    if (state.queue.size > 200) {
      const first = state.queue.keys().next().value;
      if (first) state.queue.delete(first);
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

  // UI wiring
  $("#refresh-btn").addEventListener("click", () => void refresh({ fresh: true }));
  $("#camera-shortcut").addEventListener("click", () => {
    state.stack = [
      { id: null, name: "O2 Cloud", path: "/" },
      { id: null, name: "Cámara", path: "/", virtual: "camera" },
    ];
    void refresh();
  });
  $("#load-more").addEventListener("click", () => {
    state.visibleLimit += 500;
    renderListing();
  });
  $("#upload-file-btn").addEventListener("click", () => void chooseFiles(false));
  $("#upload-folder-btn").addEventListener("click", () => void chooseFiles(true));
  $("#new-folder-btn").addEventListener("click", () => void createFolder());
  $("#settings-btn").addEventListener("click", showSettings);
  $("#settings-close").addEventListener("click", hideSettings);
  $("#session-badge").addEventListener("click", () => {
    if (state.session?.logged_in) {
      showSettings();
    } else {
      showLoginModal();
    }
  });
  $("#login-btn").addEventListener("click", () => void startLogin());
  $("#login-cancel").addEventListener("click", hideLoginModal);
  $("#save-settings").addEventListener("click", () => void saveSettings());
  $("#settings-check-update").addEventListener("click", () => void checkCliUpdate());
  $("#settings-install-update").addEventListener("click", () => void installCliUpdate());
  $("#settings-login").addEventListener("click", () => {
    hideSettings();
    showLoginModal();
  });
  $("#settings-logout").addEventListener("click", () => void logout());
  $("#cli-update-btn").addEventListener("click", () => {
    showSettings();
    void checkCliUpdate();
  });
  $("#queue-toggle").addEventListener("click", () => {
    $("#queue").classList.toggle("collapsed");
    $("#queue-chevron").textContent = $("#queue").classList.contains("collapsed") ? "▸" : "▾";
  });

  // Close modals with Escape.
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      hideSettings();
      if (state.session?.logged_in) hideLoginModal();
    }
  });
}

window.addEventListener("DOMContentLoaded", () => {
  void bootstrap();
});
