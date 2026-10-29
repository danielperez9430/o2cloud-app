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
- Login/logout through the CLI's WebView (phone + SMS).
- Finder-style window: a sidebar with the root, «Cámara» and the root folders; a path bar; search within the folder; columns sortable by name, size or date; multiple selection (click, ⌘, ⇧, ⌘A) to download or move to the O2 trash at once; a context menu; and keyboard shortcuts (listed in Settings).
- Download, delete (to the O2 trash) and create folders. Several files download into a chosen folder without replacing anything there.
- Transfers live in a popover from the toolbar, with bytes, speed and time left; the toolbar button shows how many are running.
- Drag and drop files/folders. Uploads run one at a time, in order, with progress, and skip files already in the folder with the same name and size (so retrying a folder upload doesn't duplicate it).
- Automatic chunking for files above the configured size, one part at a time, so only one part's worth of free disk is needed. An interrupted chunked upload resumes from the parts already in O2.
- Downloads go to `<name>.o2download` and are renamed into place only when complete; chunked files are checked part by part and as a whole on the way.
- Listings are cached for 5 minutes while browsing: `o2cloud ls` lists the whole account on every call (about 14 s on 46,000 files). The last few listings are also saved on disk, so the app opens with the previous data and refreshes it in the background. The refresh button, and every change made from the app, fetch a new one.
- **Virtual "Cámara" folder**: groups photos/videos by year at the UI level without moving anything in O2. Obvious non-camera media (screenshots, WhatsApp, Telegram, screen recordings, wallpapers) stays in the root.
- CLI manager: shows the current CLI version/origin, checks GitHub releases, downloads a new binary, tries it against the account and keeps the current CLI if it cannot list files.
- **Subir desde Fotos (macOS)**: a Photos view lists the library's videos, photos or both, filters by name, type, album, favorites, date range and size (including «más de 4 GB»), loads thumbnails and pages of 300, marks what is already in O2, can select only the pending ones, and exports the selected originals — iCloud-only ones included — straight into the chunked upload. Files already in the target folder with the same name and size are skipped before downloading them from iCloud, and a download can be cancelled while it runs.
- **Shared upload library**: every upload records its SHA-256 in a hidden `/.o2cloud-gui/library/` folder in O2. Each device writes an immutable snapshot and merges the snapshots from the others, so a photo or video uploaded from another Mac is recognised before exporting it again. If a match is only in another folder, the app lists the duplicates and asks, file by file, whether to skip each one or upload another copy. Settings shows the library, syncs it, reconciles it against the account and can forget this device's copy.
- Settings for chunk size and camera grouping.

## Tested status

Tested on macOS 27 (Apple Silicon) against a real O2 Spain account:

- Login via SMS: works.
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

Pending validation tests are tracked in [`PENDING_TESTS.md`](PENDING_TESTS.md).

## Requirements

- macOS 10.15 or later, Windows or Linux.
- Rust 1.85+ and Node 22+ to build.
- An O2 Cloud / Movistar Cloud account.
- The `o2cloud` CLI (bundled for macOS ARM64 in this build; on other platforms the app can download it).
- For «Subir desde Fotos»: permission to read the Photos library (macOS only).

## Build

On macOS, `app.sh` installs the dependencies when needed and runs the right command:

```bash
./app.sh dev      # development window, reloads on changes
./app.sh build    # .app
./app.sh open     # open the built app, building it first if missing
./app.sh dmg      # .app + .dmg installer
```

Or by hand:

```bash
npm install
npm run tauri dev      # development
npm run tauri build    # .app + .dmg on macOS
```

The macOS bundle is produced at:

```
src-tauri/target/release/bundle/macos/O2 Cloud.app
src-tauri/target/release/bundle/dmg/O2 Cloud_0.1.0_aarch64.dmg
```

## CLI management

The app resolves the CLI in this order:

1. `O2CLOUD_CLI` environment variable.
2. `~/Library/Application Support/…/cli/current.json` (managed update).
3. Bundled binary at `Contents/Resources/bin/<platform>/o2cloud`.
4. `o2cloud` in `PATH`.
5. `~/.local/bin` and `~/.cargo/bin`.

### Bundled macOS binary

This build bundles `o2cloud` 0.7.0 plus one upstream fix that is not released yet:

- O2 returns **negative `creationdate` values** for some old videos (e.g. `-777523141000`).
- 0.7.0 reads timestamps as `u64`, so `ls`, `find`, `download`, `rm`, `mv`, `rename`, `cp`, `share` and `unshare` fail on the whole account.
- The fix reads negative timestamps as unknown; the CLI then shows the upload date.

Reported in [o2cloud-cli#7](https://github.com/danielperez9430/o2cloud-cli/issues/7) and fixed by [o2cloud-cli#8](https://github.com/danielperez9430/o2cloud-cli/pull/8), which is still open with no review as of 27 Oct 2026. The bundled binary is built from that PR branch, which is [`patches/o2cloud-cli-0.7.0-negative-creationdate.patch`](patches/o2cloud-cli-0.7.0-negative-creationdate.patch) on top of `v0.7.0`. It still reports `o2cloud 0.7.0`. Once a release includes the fix, replace the bundled binary with the official asset and delete the patch; the CLI manager will also offer that release, and its smoke test keeps the current CLI if the new one cannot list files.

The CLI prints the result of `upload`, `mkdir` and `rm` (including the new ID) to **stderr**, so the app reads both streams.

## Known limitations

- O2's 4 GB per-file limit is enforced server-side. Chunking is a workaround, not a bypass.
- Sessions last about 24 h. When one expires, the CLI opens its own login window during the next command.
- Changes made outside the app (phone, web) show up after the refresh button, or after 5 minutes.
- O2's write eventual consistency means the file list can lag after an upload; the app schedules extra refreshes.
- O2's internal API is unofficial and can change without notice.
- The virtual camera folder is a heuristic. It may include downloaded photos and excludes some unusual camera filenames. It is a UI view; the O2 web UI still shows everything at the root.
- Chunked files are visible as parts in the O2 web/app. Do not rename or delete individual parts.
- The Photos view loads pages of 300 and requests thumbnails for the visible rows. An iCloud download can be cancelled, and the app checks for the original's size plus 128 MB of free space before downloading it.
- Photos permission is tied to the app's code signature. An ad-hoc development build may ask for it again after a rebuild.
- The shared library lives in `/.o2cloud-gui/library/` inside the O2 account. The app hides that folder, but the O2 web UI shows it and its snapshot files. Do not delete it there unless you are willing to lose the shared hashes; the app recreates the folder but not the old snapshots. The `-gui` suffix keeps the plain `.o2cloud` name free for the official app.
- Matching against files that are not in the library yet uses a whole-account listing (`ls -a`), which takes about 14 s on a large account and is cached for 5 minutes.
- Files uploaded before the library existed or outside this app are matched by original name + size (Photos also use the capture date), not by hash. Local photo/video files are hashed during preflight; other local files are not.
- The preflight conflict check applies to individual files. Folder uploads still use the target-folder name + size check.
- The conflict dialog lists every duplicate and lets each file be skipped or uploaded separately.
- Each upload batch writes a small snapshot with just its own entries. After 8 own snapshots the app publishes a full one and removes the old deltas, which are moved to the O2 trash.
- Syncing the library between two physical Macs has not been tested end to end yet.
- The macOS build is ad-hoc signed, not notarized. For personal use it runs locally; distributing it requires an Apple Developer ID.
- Only macOS ARM64 has been built and tested. The Tauri code is cross-platform; Windows/Linux need a CLI asset (the CLI currently publishes macOS, Linux x86_64 and Windows x86_64 binaries; no Linux ARM64).

## Roadmap

### Upload library: next steps

- Test a real two-Mac sync (one snapshot each, then an upload from the other).
- Empty the old snapshots left in O2's trash after compaction.
- Hash other local file types during preflight if the extra read is acceptable.
- Read hashes from existing chunk manifests to index files uploaded before the
  library existed.

### Photos: remaining validation

- Test pagination with a library over 10,000 items.
- Test cancelling a real iCloud download when nothing is cached.

### PhotoKit notes (25 Oct 2026)

What a manual run showed with three videos of 4.4–7.2 GB on an "Optimize Mac Storage" library:

- AppleScript `export … with using originals` returns without error but writes nothing when the original is only in iCloud.
- With a VPN on, every iCloud download failed, in Photos itself too, while playback still streamed. PhotoKit reports this as `PHPhotosErrorDomain` 3169 (network error).
- `cloudphotod` downloads the whole original to `~/Library/Caches/CloudKit/com.apple.cloudphotod/…/MMCS/` before handing over any data, so each video needs about its size in free disk on top of the upload's part file. Large libraries need one asset at a time and a free-space check.
- The asset's original size is in the Photos database (`ZADDITIONALASSETATTRIBUTES.ZORIGINALFILESIZE`); PhotoKit only exposes it through `PHAssetResource`'s undocumented `fileSize` key.

## Building the bundled CLI

```bash
git clone https://github.com/danielperez9430/o2cloud-cli.git
cd o2cloud-cli
git checkout v0.7.0
git am /path/to/o2cloud-app/patches/o2cloud-cli-0.7.0-negative-creationdate.patch
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
```

## License

The app code is MIT. The bundled `o2cloud-cli` is MIT; see its repository for attribution.
