#!/usr/bin/env node
// Keep every version file in sync. Run from anywhere:
//   npm run release:version -- 0.2.0
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const next = process.argv[2];

const SEMVER =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

if (!next || !SEMVER.test(next)) {
  fail("usage: npm run release:version -- <version>  (for example: 0.2.0)");
}

function read(relative) {
  return readFileSync(resolve(root, relative), "utf8");
}

function write(relative, contents) {
  writeFileSync(resolve(root, relative), contents);
}

function readJson(relative) {
  return JSON.parse(read(relative));
}

function writeJson(relative, value) {
  write(relative, `${JSON.stringify(value, null, 2)}\n`);
}

function replaceOnce(relative, pattern, replacement) {
  const before = read(relative);
  const matches = before.match(pattern);
  if (!matches) fail(`could not find the version in ${relative}`);
  const after = before.replace(pattern, replacement);
  if (after === before) fail(`the version in ${relative} did not change`);
  write(relative, after);
}

const current = readJson("src-tauri/tauri.conf.json").version;
if (current === next) fail(`the version is already ${next}`);

const packageJson = readJson("package.json");
packageJson.version = next;
writeJson("package.json", packageJson);

const packageLock = readJson("package-lock.json");
packageLock.version = next;
packageLock.packages[""].version = next;
writeJson("package-lock.json", packageLock);

const tauriConfig = readJson("src-tauri/tauri.conf.json");
tauriConfig.version = next;
writeJson("src-tauri/tauri.conf.json", tauriConfig);

replaceOnce(
  "src-tauri/Cargo.toml",
  /(\[package\]\nname\s*=\s*"o2cloud-app"\nversion\s*=\s*")[^"]+(")/,
  `$1${next}$2`,
);

replaceOnce(
  "src-tauri/Cargo.lock",
  /(\[\[package\]\]\nname\s*=\s*"o2cloud-app"\nversion\s*=\s*")[^"]+(")/,
  `$1${next}$2`,
);

console.log(`${current} -> ${next}`);
console.log(
  "Updated package.json, package-lock.json, src-tauri/Cargo.toml, src-tauri/Cargo.lock and src-tauri/tauri.conf.json.",
);
console.log("Next: npm run build && (cd src-tauri && cargo test --locked)");
