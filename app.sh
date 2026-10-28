#!/usr/bin/env bash
# Run, build or open the O2 Cloud desktop app on macOS.
set -euo pipefail

cd "$(dirname "$0")"

APP_PATH="src-tauri/target/release/bundle/macos/O2 Cloud.app"
DMG_DIR="src-tauri/target/release/bundle/dmg"

usage() {
  cat <<EOF
Uso: ./app.sh <comando>

  dev     Abre la app en modo desarrollo; se recarga al cambiar el código
  build   Compila la app final (.app)
  open    Abre la app compilada (la compila antes si aún no existe)
  dmg     Compila la app y su instalador .dmg, para pasársela a otros
EOF
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Falta '$1'. $2" >&2
    exit 1
  fi
}

prepare() {
  need node "Instálalo con: brew install node@22"
  need npm "Viene con Node: brew install node@22"
  need cargo "Instala Rust con: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

  # Install JS dependencies on the first run, or when package.json or the lockfile changed since.
  if [ ! -f node_modules/.package-lock.json ] \
    || [ package.json -nt node_modules/.package-lock.json ] \
    || [ package-lock.json -nt node_modules/.package-lock.json ]; then
    echo "→ Instalando dependencias…"
    npm install
  fi
}

dev() {
  prepare
  echo "→ Arrancando en modo desarrollo (Ctrl+C para cerrar)…"
  npm run tauri dev
}

# Tauri leaves the bundle linker-signed, with Info.plist unbound. macOS uses
# the bundle signature for TCC, so the Photos permission prompt never appears
# (and the request hangs) until the bundle is signed as a whole.
sign_app() {
  echo "→ Firmando la app (ad-hoc)…"
  codesign --force --deep --sign - --identifier es.dabi.o2cloud "$APP_PATH"
}

build() {
  prepare
  echo "→ Compilando (la primera vez tarda varios minutos)…"
  npm run tauri build -- --bundles app
  sign_app
  echo
  echo "Listo: $APP_PATH"
}

# The .dmg step mounts a disk image, which macOS sometimes keeps busy for a
# moment and fails; it's only needed to hand the app to someone else.
dmg() {
  prepare
  echo "→ Compilando la app y su instalador…"
  npm run tauri build -- --bundles app,dmg
  sign_app
  echo
  echo "Listo:"
  echo "  App: $APP_PATH"
  for file in "$DMG_DIR"/*.dmg; do
    if [ -e "$file" ]; then echo "  DMG: $file"; fi
  done
}

open_app() {
  [ -d "$APP_PATH" ] || build
  sign_app
  open "$APP_PATH"
}

case "${1:-}" in
  dev) dev ;;
  build) build ;;
  open) open_app ;;
  dmg) dmg ;;
  "" | -h | --help | help) usage ;;
  *)
    echo "Comando desconocido: $1" >&2
    echo >&2
    usage >&2
    exit 1
    ;;
esac
