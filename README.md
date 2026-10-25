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
- Browse folders, download, delete (to O2 trash), create folders.
- Drag and drop files/folders; upload queue with progress.
- Automatic chunking for files above the configured size.
- **Virtual "Cámara" folder**: groups photos/videos by year at the UI level without moving anything in O2. Obvious non-camera media (screenshots, WhatsApp, Telegram, screen recordings, wallpapers) stays in the root.
- CLI manager: shows the current CLI version/origin, checks GitHub releases, downloads a new binary, smoke-tests it against the account and rolls back if it cannot list files.
- Settings for chunk size and camera grouping.

## Tested status

Tested on macOS 27 (Apple Silicon) against a real O2 Spain account:

- Login via SMS: works.
- 20 MB upload: works.
- 2 MB upload through the app: works; queue reaches `Subido (id …)`.
- **4.2 GB upload: rejected by O2 after the full transfer** with `MED-1020`. This confirms chunking is required.
- `.o2part.001` and `.o2manifest.json` filenames are accepted by O2 and preserved in listings.
- O2 is eventually consistent on writes: new files can take ~10–60 s to appear in `ls`.
- Virtual camera folder: 46,433 items / ~525 GB classified; 162 non-camera items left in the root (on the test account).

## Requirements

- macOS, Windows or Linux.
- Rust 1.85+ and Node 22+ to build.
- An O2 Cloud / Movistar Cloud account.
- The `o2cloud` CLI (bundled for macOS ARM64 in this build; on other platforms the app can download it).

## Build

```bash
npm install
npm run tauri dev      # development
npm run tauri build    # .app + .dmg on macOS
```

The macOS bundle is produced at:

```
src-tauri/target/release/bundle/macos/o2cloud-app.app
src-tauri/target/release/bundle/dmg/o2cloud-app_0.1.0_aarch64.dmg
```

## CLI management

The app resolves the CLI in this order:

1. `O2CLOUD_CLI` environment variable.
2. `~/Library/Application Support/…/cli/current.json` (managed update).
3. Bundled binary at `Contents/Resources/bin/<platform>/o2cloud`.
4. `o2cloud-patched` / `o2cloud` in `PATH`.
5. `~/.local/bin` and `~/.cargo/bin`.

### Bundled macOS binary

This build bundles a locally patched `o2cloud` 0.7.0 (`0.7.0-dabi.1`). The patch fixes a real parser bug found on the test account:

- O2 returns **negative `creationdate` values** for some old videos (e.g. `-777523141000`).
- Upstream 0.7.0 deserializes timestamps as `u64` and aborts the entire listing.
- The patched build treats negative timestamps as unknown and also tolerates negative internal IDs.

The patch is in `o2cloud-cli`'s `src/api.rs` (`deserialize_optional_timestamp`). It should be sent upstream; until then, the app's update smoke test will reject a new upstream release that reintroduces the bug and keep the previous CLI.

The CLI prints most operation results to **stderr** (upload/mkdir/delete IDs). The app captures both streams; this was another upstream integration bug fixed here.

## Known limitations

- O2's 4 GB per-file limit is enforced server-side. Chunking is a workaround, not a bypass.
- The app cannot upload a file while the O2 session is expired; it uses the CLI's ~24 h session. Re-login when prompted.
- O2's write eventual consistency means the file list can lag after an upload; the app schedules extra refreshes.
- O2's internal API is unofficial and can change without notice.
- The virtual camera folder is a heuristic. It may include downloaded photos and excludes some unusual camera filenames. It is a UI view; the O2 web UI still shows everything at the root.
- Chunked files are visible as parts in the O2 web/app. Do not rename or delete individual parts.
- The macOS build is ad-hoc signed, not notarized. For personal use it runs locally; distributing it requires an Apple Developer ID.
- Only macOS ARM64 has been built and tested. The Tauri code is cross-platform; Windows/Linux need a CLI asset (the CLI currently publishes macOS, Linux x86_64 and Windows x86_64 binaries; no Linux ARM64).

## CLI patch summary

The local fork changes `src/api.rs`:

- `MediaItem.date` / `creationdate`: accept negative numbers as `None` instead of failing.
- Negative IDs are treated as internal pseudo-items and filtered out.

Build it with:

```bash
git clone https://github.com/danielperez9430/o2cloud-cli.git
# apply the patch in this repo's notes
cargo build --release
```

## Project layout

```
src/                    Tauri frontend (vanilla TS)
src-tauri/src/          Rust backend
  cli.rs                CLI discovery, execution, updates
  transfer.rs           chunking, SHA-256, upload/download
  camera.rs             virtual camera classification
  commands.rs           Tauri commands
src-tauri/bin/          bundled CLI binaries
```

## License

The app code is MIT. The bundled `o2cloud-cli` is MIT; see its repository for attribution.
