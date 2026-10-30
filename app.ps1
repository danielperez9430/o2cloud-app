#!/usr/bin/env pwsh
# Run, build or open the O2 Cloud desktop app on Windows.
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
Set-Location $PSScriptRoot

$AppPath = "src-tauri\target\release\o2cloud-app.exe"
$BundleDir = "src-tauri\target\release\bundle"

function Show-Usage {
  @"
Uso: .\app.ps1 <comando>

  dev        Abre la app en modo desarrollo; se recarga al cambiar el código
  build      Compila la app final (.exe)
  open       Abre la app compilada (la compila antes si aún no existe)
  installer  Compila la app y sus instaladores .exe/.msi, para pasársela a otros
"@
}

function Fail($Message) {
  [Console]::Error.WriteLine($Message)
  exit 1
}

function Require-Command($Name, $Hint) {
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    Fail "Falta '$Name'. $Hint"
  }
}

function Add-CargoToPath {
  # rustup adds this directory to new terminals, but an already-open one may
  # not have it yet.
  $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
  if ((Test-Path (Join-Path $cargoBin "cargo.exe")) -and ($env:Path -notlike "*$cargoBin*")) {
    $env:Path = "$cargoBin;$env:Path"
  }
}

function Invoke-Prepare {
  Add-CargoToPath
  Require-Command node "Instálalo con: winget install OpenJS.NodeJS.LTS"
  Require-Command npm "Viene con Node: winget install OpenJS.NodeJS.LTS"
  Require-Command cargo "Instala Rust con: winget install Rustlang.Rustup"

  # Install JS dependencies on the first run, or when package.json or the
  # lockfile changed since.
  $stamp = Join-Path $PSScriptRoot "node_modules\.package-lock.json"
  $install = -not (Test-Path $stamp)
  if (-not $install) {
    $stampTime = (Get-Item $stamp).LastWriteTimeUtc
    $install =
      ((Get-Item "package.json").LastWriteTimeUtc -gt $stampTime) -or
      ((Get-Item "package-lock.json").LastWriteTimeUtc -gt $stampTime)
  }
  if ($install) {
    Write-Host "→ Instalando dependencias…"
    npm install
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  }
}

function Invoke-Dev {
  Invoke-Prepare
  Write-Host "→ Arrancando en modo desarrollo (Ctrl+C para cerrar)…"
  npm run tauri dev
  exit $LASTEXITCODE
}

function Invoke-Build {
  Invoke-Prepare
  Write-Host "→ Compilando (la primera vez tarda varios minutos)…"
  npm run tauri build -- --no-bundle
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  Write-Host ""
  Write-Host "Listo: $AppPath"
}

function Invoke-Installer {
  Invoke-Prepare
  Write-Host "→ Compilando la app y sus instaladores…"
  npm run tauri build
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  Write-Host ""
  Write-Host "Listo:"
  Write-Host "  App: $AppPath"
  Get-ChildItem (Join-Path $BundleDir "nsis\*.exe") -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "  Instalador: $($_.FullName)" }
  Get-ChildItem (Join-Path $BundleDir "msi\*.msi") -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "  MSI: $($_.FullName)" }
}

function Open-App {
  if (-not (Test-Path $AppPath)) { Invoke-Build }
  Start-Process -FilePath (Resolve-Path $AppPath)
}

$command = if ($args.Count -gt 0) { $args[0] } else { "" }

switch ($command) {
  "dev" { Invoke-Dev }
  "build" { Invoke-Build }
  "open" { Open-App }
  "installer" { Invoke-Installer }
  { $_ -in "", "-h", "--help", "help" } { Show-Usage }
  default {
    Fail "Comando desconocido: $command`n`n$(Show-Usage)"
  }
}
