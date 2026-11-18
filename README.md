# O2 Cloud Desktop

A native, cross-platform desktop client for **O2 Cloud / Movistar Cloud** (Telefónica España), built with **Tauri 2 + Rust** and backed by the [`o2cloud-cli`](https://github.com/danielperez9430/o2cloud-cli) binary.

The app is a graphical layer on top of the CLI: it does not reimplement O2's private API. That keeps authentication, uploads and downloads in one audited place, and lets the app update the CLI independently.

## Why this exists

O2 Cloud gives 10 TB but enforces a **4 GB limit per file** on the server. The official apps and web UI stop there. The gateway/WebDAV approach does not bypass it either: the rejection comes from O2 (`MED-1020: File size exceeds configured max file size limit`).

This app keeps the convenience of a desktop client and adds a **transparent chunking layer** for files larger than the configured threshold (3.5 GB by default):

```
movie.mkv                         -> logical file shown in the app
movie.mkv.o2part.001              -> 3.5 GB chunk
movie.mkv.o2part.002              -> remainder
movie.mkv.o2manifest.json         -> sizes + SHA-256 hashes
```

Each chunk is uploaded as a normal file below O2's limit. On download, the app fetches the manifest, verifies each chunk, concatenates them and verifies the whole-file SHA-256. The chunks are raw bytes, so `cat movie.mkv.o2part.* > movie.mkv` also reconstructs the file without the app.

The O2 web UI will show the chunks and the manifest. The app hides them behind a single logical file.

## Features

- Native Tauri UI, no Docker.
- Login/logout through an O2 login window (phone + SMS); the CLI validates and stores the session.
- Finder-style window: a sidebar with the root, «Cámara» and the root folders; a path bar; search within the folder; columns sortable by name, size or date; multiple selection (click, ⌘/Ctrl, ⇧, ⌘A/Ctrl+A) to download or move to the O2 trash at once; a context menu; and keyboard shortcuts (listed in Settings, with the right symbols on each platform).
- Download, delete (to the O2 trash) and create folders. Several files download into a chosen folder without replacing anything there.
- Transfers live in a popover from the toolbar, with bytes, speed and time left; the toolbar button shows how many are running.
- Drag and drop files/folders. Uploads run one at a time, in order, with progress, and skip files already in the folder with the same name and size (so retrying a folder upload doesn't duplicate it).
- Automatic chunking. New files over 4 GB use the CLI's native split (`name.001`, …) and get an app-written `name.o2split.json` with whole-file and per-part SHA-256; smaller files still use the app's `.o2part` format above the configured size while the hybrid is evaluated. An interrupted upload resumes from the parts already in O2.
- Downloads go to `<name>.o2download` and are renamed into place only when complete; `.o2part` files are checked part by part and as a whole, and CLI splits are joined by the CLI and checked against the manifest's whole-file SHA-256.
- Listings are cached for 5 minutes while browsing: `o2cloud ls` lists the whole account on every call (about 14 s on 46,000 files). The last few listings are also saved on disk, so the app opens with the previous data and refreshes it in the background. The refresh button, and every change made from the app, fetch a new one.
- **Virtual "Cámara" folder**: groups photos/videos by year at the UI level without moving anything in O2. Obvious non-camera media (screenshots, WhatsApp, Telegram, screen recordings, wallpapers) stays in the root.
- CLI manager: shows the current CLI version/origin, checks GitHub releases, downloads a new binary, tries it against the account and keeps the current CLI if it cannot list files.
- **Subir desde Fotos (macOS)**: a Photos view lists the library's videos, photos or both, filters by name, type, album, favorites, date range and size (including «más de 4 GB»), loads thumbnails and pages of 300, marks what is already in O2, can select only the pending ones, and exports the selected originals — iCloud-only ones included — straight into the chunked upload. The date range is prefilled with the last month and enabled with the «Por fecha» checkbox; «Limpiar» removes it. Files already in the target folder with the same name and size are skipped before downloading them from iCloud, and a download can be cancelled while it runs.
- **Shared upload library**: every upload records its SHA-256 in a hidden `/.o2cloud-gui/library/` folder in O2. Each device writes an immutable snapshot and merges the snapshots from the others, so a photo or video uploaded from another Mac is recognised before exporting it again. An optional Settings toggle also hashes non-media files before upload, so renamed documents are matched by content too. If a match is only in another folder, the app lists the duplicates and asks, file by file, whether to skip each one or upload another copy. Settings shows the library, syncs it, reconciles it against the account, forgets this device's copy and can index `.o2manifest.json` or `.o2split.json` files to recover their hashes.
- Settings for chunk size (used by the `.o2part` path up to 4 GB while the hybrid is evaluated) and camera grouping.

## Tested status

Tested on macOS 27 (Apple Silicon) against a real O2 Spain account:

- Login via SMS: works through the CLI's login window; the app's own login window (same OAuth flow) still needs an end-to-end run.
- 20 MB upload: works.
- 2 MB upload through the app: works; queue reaches `Subido (id …)`.
- A raw 4.2 GB single-file upload is rejected by O2 after the full transfer with `MED-1020`; the limit is server-side.
- **4.2 GB upload through the app: works.** Split into two parts (3.5 GB + 0.7 GB), both accepted; the manifest SHA-256 matched the local file byte for byte.
- Three real videos already stored this way in `/Vídeos grandes`: 4.4 GB, 6.6 GB and 7.2 GB, each split into two parts.
- `.o2part.001` and `.o2manifest.json` filenames are accepted by O2 and preserved in listings.
- O2 is eventually consistent on writes: new files can take ~10–60 s to appear in `ls`.
- Virtual camera folder: 46,433 items / ~525 GB classified. The 161 non-camera root files were moved to `/Otros`, leaving the root clean.
- PhotoKit (standalone harness, macOS 27): listed the library's videos with their original filenames and sizes, and exported one original byte for byte. The permission prompt and the full Photos view still need a run in the bundled app.
- Upload library: a snapshot was uploaded to, listed in, downloaded from and deleted from the real `/.o2cloud-gui/library/` folder; merge, fingerprint and prune unit tests pass. Syncing between two physical devices still needs an end-to-end run.
- macOS bundle: the debug `.app` carries `NSPhotoLibraryUsageDescription` and `LSMinimumSystemVersion` 10.15, so the Photos permission is declared where TCC looks for it.

The bundled CLI was updated to **0.10.0** after these runs, and its smoke test
passed on 18 Nov 2026 against a real account: `status`, `quota`, `mkdir`,
upload/download/rm, a split upload with `--part-size 10000`, the join by
SHA-256 and the resume with `--skip-existing`. On the same ~46k-file account,
0.10.0 cut `download`/`rm` by ID from ~12 s each to 3 requests (~0.4 s), and an
app-like chunked download/delete (manifest + 2 parts) from ~36 s to about
**1.1 s / 9 requests**. The macOS flows above were last exercised with 0.8.0;
a real >4 GB hybrid upload and a 3.5–4 GB `.o2part` upload are still tracked
in [`PENDING_TESTS.md`](PENDING_TESTS.md).

Tested on Windows 11 (x86_64) with the `o2cloud-cli` 0.8.0 Windows build:

- `cargo test` (39 tests), `npm run build`, the debug app and the release bundles build and run.
- The app finds `o2cloud.exe` through `O2CLOUD_CLI`, a managed download or `PATH`, shows its version and reports `Not logged in` as expected.
- The Settings update flow downloaded and installed the `o2cloud-cli` 0.8.0 Windows build (without a session, so it skipped the account test), wrote `current.json` and switched to it. The CLI's own login window («O2 Cloud - Iniciar sesión») opens and responds.
- The NSIS installer installs, the installed app finds the managed CLI, and the uninstaller removes it cleanly.
- Real O2 account on Windows: SMS login, 46,463-item virtual camera view, a 5,321-file folder, a 32.3 MB download, a 79 B upload with spaces/`ñ` and a byte-identical download, a 300 MB chunked upload (256 MB + 44 MB) with a matching SHA-256 on download, and a folder upload that skipped a hidden file. The test files were deleted afterwards.
- UI checks on Windows: folder navigation, search, `Ctrl+F`/`Ctrl+A`/`Esc`, context menu, Settings/update check, library sync, the transfers panel and the native «Guardar archivo» dialog all work.
- Windows-specific paths were fixed and exercised: invalid download names, atomic cache saves, hidden/system files in folder uploads, temporary chunk names and the upload progress watchdog.
- Still pending on Windows: cancelling/resuming a >3.5 GB chunked upload, session expiry, native drag and drop, and the installer bootstrap on a machine without WebView2.

Tested on Linux x86_64 under WSL2 (Ubuntu 24.04, WSLg):

- `cargo check --all-targets`, `cargo test` (38 tests), `npm run build` and the debug app build and run.
- The app opened under WSLg with a real account in read-only mode: root listing, folder rows, search and keyboard navigation.
- The published `o2cloud-cli` 0.8.0 Linux binary requires GLIBC 2.43 and fails on Ubuntu 24.04 (2.39); the CLI was built from source instead.
- The CLI's own login window does not work under WSLg; an existing `~/.config/o2cli/auth.json` was used for the read-only test. See [`LINUX.md`](LINUX.md) for the setup and details.

Pending validation tests are tracked in [`PENDING_TESTS.md`](PENDING_TESTS.md). See [`WINDOWS.md`](WINDOWS.md) for the Windows audit and [`LINUX.md`](LINUX.md) for the WSL2/Linux notes.

## Requirements

- macOS 10.15 or later, Windows 10/11 or Linux.
- Rust 1.85+ and Node 22+ to build.
- An O2 Cloud / Movistar Cloud account.
- The `o2cloud` CLI from [`o2cloud-cli`](https://github.com/danielperez9430/o2cloud-cli) (bundled for macOS ARM64 in this build; on Windows the app downloads the `.zip` published by that project on GitHub on first run).
- On Windows: Microsoft Edge WebView2 Runtime (included in Windows 10/11; the installer can bootstrap it) and the Visual Studio 2022 Build Tools with the «Desktop development with C++» workload for the MSVC Rust target.
- On Linux: WebKitGTK 4.1 and GTK 3 development packages; the full `apt` command is in [`LINUX.md`](LINUX.md).
- For «Subir desde Fotos»: permission to read the Photos library (macOS only).

## Build

On macOS, `app.sh` installs the dependencies when needed and runs the right command:

```bash
./app.sh dev      # development window, reloads on changes
./app.sh build    # .app
./app.sh open     # open the built app, building it first if missing
./app.sh dmg      # .app + .dmg installer
```

On Windows, `app.ps1` does the same:

```powershell
.\app.ps1 dev        # development window, reloads on changes
.\app.ps1 build      # release .exe
.\app.ps1 open       # open the built app, building it first if missing
.\app.ps1 installer  # .exe + .msi installers
```

If PowerShell blocks the script, run
`powershell -ExecutionPolicy Bypass -File .\app.ps1 dev` (or `pwsh` if you use
PowerShell 7).

Or by hand:

```bash
npm install
npm run tauri dev      # development
npm run tauri build    # .app + .dmg on macOS, .exe + .msi on Windows
```

The macOS bundle is produced at:

```
src-tauri/target/release/bundle/macos/O2 Cloud.app
src-tauri/target/release/bundle/dmg/O2 Cloud_0.1.0_aarch64.dmg
```

The Windows build is produced at:

```
src-tauri\target\release\o2cloud-app.exe
src-tauri\target\release\bundle\nsis\O2 Cloud_0.1.0_x64-setup.exe
src-tauri\target\release\bundle\msi\O2 Cloud_0.1.0_x64_en-US.msi
```

The Windows installers are not code-signed, so SmartScreen may warn the first
time they are run. Building from source avoids the installers entirely:
`npm run tauri build -- --no-bundle` only produces the `.exe`.

## CLI management

The app requires `o2cloud` **0.9.0 or newer**. **0.10.0 is recommended** and is
the version bundled on macOS: it makes `download` and `rm` by ID or path take
3 requests instead of listing the whole account (~12 s on 46k files), so the
app's chunked transfers no longer pay a full listing per part. 0.9.0 added the
download-integrity, SAPI and fresh-ID fixes; 0.8.0 and older are detected but
not used, and Settings offers the update.

0.9.0 also added native split uploads (`name.001`, `name.002`, joined again by
`o2cloud download name`), `--skip-existing` and parallel `upload-dir`. The app
now uses that mode for **new files over 4 GB**: the CLI splits and joins them,
and the app adds `name.o2split.json` with the whole-file and per-part SHA-256,
so the upload library and Photos deduplication keep working. Files up to 4 GB
still use the app's `.o2part.NNN` + `.o2manifest.json` chunking when they exceed
the configured chunk size while the hybrid is evaluated; existing `.o2part`
files remain readable either way. A file uploaded from the CLI in parts is
grouped by the app when it has a `name.o2split.json` manifest or its first part
is large enough to be a real split; otherwise it appears as its individual
`.001` files.

### File format compatibility

Existing `.o2part.NNN` + `.o2manifest.json` files stay readable, downloadable
and deletable; no migration is required. New files over 4 GB use
`name.001` + `name.o2split.json`. The app decides per file from the part names,
so both formats can coexist in the same folder, and the shared library indexes
both manifest types.

The app resolves the CLI in this order:

1. `O2CLOUD_CLI` environment variable.
2. Managed update: `~/Library/Application Support/es.dabi.o2cloud/cli/current.json` on macOS, `%APPDATA%\es.dabi.o2cloud\cli\current.json` on Windows.
3. Bundled binary: `Contents/Resources/bin/<platform>/o2cloud` on macOS, `bin\windows-x86_64\o2cloud.exe` on Windows.
4. `o2cloud` in `PATH`.
5. `~/.local/bin` and `~/.cargo/bin`.

This Windows build does not bundle the CLI. On first run Settings opens and
**Buscar actualización** downloads the
`o2cloud-x86_64-pc-windows-msvc.zip` release published by `o2cloud-cli`, tests
it against the account when there is an active session and makes it current.

On Linux, the published 0.9.0 and 0.10.0 binaries still require GLIBC 2.43 and
do not start on Ubuntu 24.04 (GLIBC 2.39). Build `o2cloud-cli` 0.10.0 from
source and place the binary at `src-tauri/bin/linux-x86_64/o2cloud`;
[`LINUX.md`](LINUX.md) has the commands.

An older CLI is kept only as a last resort, so the app can show which version
needs updating.

### Bundled macOS binary

This build bundles the official `o2cloud` **0.10.0** binary for Apple Silicon,
downloaded from its GitHub release. It includes the earlier fix for accounts
with negative `creationdate` values ([o2cloud-cli#8](https://github.com/danielperez9430/o2cloud-cli/pull/8)),
the download/ID/SAPI fixes from 0.9.0 and the by-ID/path fast path from 0.10.0,
so no local patch is applied.

With `--json`, the payload (including new IDs) is a single JSON value on
stdout; progress stays on stderr.

## Known limitations

- O2's 4 GB per-file limit is enforced server-side. Chunking is a workaround, not a bypass.
- Sessions last about 24 h. When one expires, the app stops the CLI before it can open its own login window (that window crashes on macOS 27) and asks you to sign in again in the app's own window.
- Changes made outside the app (phone, web) show up after the refresh button, or after 5 minutes.
- O2's write eventual consistency means the file list can lag after an upload; the app schedules extra refreshes.
- O2's internal API is unofficial and can change without notice.
- The virtual camera folder is a heuristic. It may include downloaded photos and excludes some unusual camera filenames. It is a UI view; the O2 web UI still shows everything at the root.
- Chunked files are visible as parts in the O2 web/app. Do not rename or delete individual parts.
- The Photos view loads pages of 300 and requests thumbnails for the visible rows; counting the library for the summary walks every asset, so the first «Fotos y vídeos» page takes about 1–2 minutes with 52,000 items. An iCloud download can be cancelled, and the app checks for the original's size plus 128 MB of free space before downloading it.
- Photos permission is tied to the app's code signature. An ad-hoc development build may ask for it again after a rebuild.
- Hashing non-media files before upload is opt-in. It reads each selected file once, applies to individual files (folders keep the name+size check) and, when the selection has no media, uses the shared library instead of a whole-account scan.
- The shared library lives in `/.o2cloud-gui/library/` inside the O2 account. The app hides that folder, but the O2 web UI shows it and its snapshot files. Do not delete it there unless you are willing to lose the shared hashes; the app recreates the folder but not the old snapshots. The `-gui` suffix keeps the plain `.o2cloud` name free for the official app.
- Matching against files that are not in the library yet uses a whole-account listing (`ls -a`), which takes about 14 s on a large account and is cached for 5 minutes.
- Files uploaded before the library existed or outside this app are matched by original name + size (Photos also use the capture date), not by hash. Local photo/video files are hashed during preflight when that is cheap (up to 512 MB, or larger if a same-size file already exists); other local files are not hashed unless the opt-in is on.
- The preflight conflict check applies to individual files. Folder uploads still use the target-folder name + size check.
- Preflight hashing is skipped for individual files over 512 MB unless O2 or the shared library already has a file of the same size: reading a 20 GB video from a slow external disk before starting the upload took minutes per file and made the app look stuck. A unique size cannot be a content duplicate, so those files are matched by name + size, and their SHA-256 is recorded after the upload.
- The conflict dialog lists every duplicate and lets each file be skipped or uploaded separately.
- Each upload batch writes a small snapshot with just its own entries. After 8 own snapshots the app publishes a full one and removes the old deltas, which are moved to the O2 trash.
- Syncing the library between two physical Macs has not been tested end to end yet.
- The macOS build is ad-hoc signed, not notarized. For personal use it runs locally; distributing it requires an Apple Developer ID.
- The Windows installers are not code-signed. SmartScreen may warn; for personal use, build from source or use the unsigned installer.
- macOS ARM64 is the main tested target. Windows x86_64 builds, its unit tests pass and the main flows were tested against a real O2 account; very large chunked transfers, cancellation/resume and session expiry still need a Windows run. Linux x86_64 builds and runs under WSL2, but uploads/downloads, the CLI login window and native desktop packaging still need a Linux run. The CLI publishes macOS, Linux x86_64 and Windows x86_64 binaries (no Linux ARM64).
- The published `o2cloud-cli` Linux 0.8.0, 0.9.0 and 0.10.0 binaries need GLIBC 2.43; Ubuntu 24.04 has 2.39, so build the CLI from source there. Under WSLg, the CLI's login window fails; use a native Linux desktop or an existing `~/.config/o2cli/auth.json`.
- On Windows, upload progress uses the CLI process's total read bytes because Windows does not expose another process's open file handles to a normal process. The percentage is approximate and clamped to the file size; stall detection still works. Downloads and chunked uploads are otherwise the same as on macOS.
- Windows rejects some names that O2 accepts (`<>:"/\|?*`, `CON`, `PRN`, trailing dots/spaces, very long names). Downloads replace those characters and the saved file may have a slightly different name than the one shown in O2. Chunk temporary files use app-controlled names, so a remote part name cannot escape the download folder.

## Roadmap

### OTA updates: GUI and CLI

- Add Tauri's updater plugin for the GUI: sign the update manifest and each
  artifact (macOS `.app`/`.dmg`, Windows `.exe`/`.msi`, Linux AppImage/deb),
  check for updates on launch and periodically, download in the background and
  apply on restart.
- Keep the managed CLI updater and show both versions in Settings. A GUI update
  must not switch the CLI version on its own, and neither update should run
  while a transfer is active.
- Publish a per-platform `latest.json` (stable/beta channels), keep the
  previous version for rollback, and refuse downgrades.
- Prerequisites: Apple Developer ID and notarization for macOS (today the app
  is ad-hoc signed), code signing for Windows, and signed Linux packages.
- Preserve settings, the upload library, cached listings and the O2 session
  across updates; verify that the new GUI can still list files before applying.
- Add a manual «Buscar actualizaciones» path for both GUI and CLI, with clear
  states (checking, downloading, ready to restart, failed).
- Test: update from the previous release on macOS/Windows/Linux, offline and
  interrupted downloads, invalid signatures, downgrade attempts, update while
  an upload/download is running, and GUI/CLI version-mismatch handling.

### CLI split support: next steps

- Stage 1 (done): read-only support for `name.001`, `name.002`, … groups plus
  an optional `name.o2split.json` manifest: group them as one logical file,
  download them with a single `o2cloud download <base>` (the CLI joins them),
  verify the whole-file SHA-256 when the manifest exists, and delete all parts
  together.
- Stage 2 (prototype in place): new files over 4 GB use the CLI's native split
  (`upload --skip-existing`) and get `name.o2split.json` with the whole-file
  and per-part SHA-256. Smaller files still use the app's chunking while this
  is evaluated; the app hashes the file once after the CLI upload and uses
  the CLI's part layout for the manifest.
- Stage 3 (after evaluation): send every file above the chunk threshold to the
  CLI split, remove the app's own upload path and the chunk-size setting, and
  keep read/download support for old `.o2part` files.
- Validate with a real file over 4 GB on macOS, Windows and Linux, and compare
  disk use, upload time and restart behaviour against the old chunking.
  Deletion still costs one `rm` per part until the upstream CLI supports
  deleting a split group by base name.

### Upload library: next steps

- Test a real two-Mac sync (one snapshot each, then an upload from the other).
- Empty the old snapshots left in O2's trash after compaction.

### Photos: remaining validation

- Test cancelling a real iCloud download when nothing is cached.

### Photos: performance

- Count the library with `PHFetchResult.count` and `PHFetchOptions` predicates
  instead of walking all 52,000 assets; the first «Fotos y vídeos» page takes
  about 1–2 minutes.

### PhotoKit notes (25 Oct 2026)

What a manual run showed with three videos of 4.4–7.2 GB on an "Optimize Mac Storage" library:

- AppleScript `export … with using originals` returns without error but writes nothing when the original is only in iCloud.
- With a VPN on, every iCloud download failed, in Photos itself too, while playback still streamed. PhotoKit reports this as `PHPhotosErrorDomain` 3169 (network error).
- `cloudphotod` downloads the whole original to `~/Library/Caches/CloudKit/com.apple.cloudphotod/…/MMCS/` before handing over any data, so each video needs about its size in free disk on top of the upload's part file. Large libraries need one asset at a time and a free-space check.
- The asset's original size is in the Photos database (`ZADDITIONALASSETATTRIBUTES.ZORIGINALFILESIZE`); PhotoKit only exposes it through `PHAssetResource`'s undocumented `fileSize` key.

## Building the bundled CLI

Use the official release asset:

```bash
gh release download v0.10.0 --repo danielperez9430/o2cloud-cli \
  --pattern o2cloud-aarch64-apple-darwin.tar.gz
tar -xzf o2cloud-aarch64-apple-darwin.tar.gz
cp o2cloud /path/to/o2cloud-app/src-tauri/bin/macos-aarch64/o2cloud
```

Or build it from source:

```bash
git clone https://github.com/danielperez9430/o2cloud-cli.git
cd o2cloud-cli
git checkout v0.10.0
cargo build --release
cp target/release/o2cloud /path/to/o2cloud-app/src-tauri/bin/macos-aarch64/o2cloud
```

## Project layout

```
src/                    Tauri frontend (vanilla TS)
src-tauri/src/          Rust backend
  cache.rs              listing cache
  cli.rs                CLI discovery, execution, updates
  transfer.rs           chunking, SHA-256, upload/download
  camera.rs             virtual camera classification
  photos.rs             Photos library commands and export (PhotoKit)
  photos.m              Objective-C PhotoKit bridge
  library.rs            shared SHA-256 upload library and O2 snapshots
  commands.rs           Tauri commands
src-tauri/Info.plist    NSPhotoLibraryUsageDescription (Photos permission)
src-tauri/bin/          bundled CLI binaries
app.sh                  macOS build/run helper
app.ps1                 Windows build/run helper
WINDOWS.md              Windows audit, fixes and manual test checklist
LINUX.md                Linux/WSL2 setup and test notes
```

## License

The app code is MIT. The bundled `o2cloud-cli` is MIT; see its repository for attribution.
