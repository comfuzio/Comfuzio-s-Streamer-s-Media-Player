#!/usr/bin/env node
// Keeps the app version identical in package.json, Cargo.toml and tauri.conf.json.
//
//   node scripts/version.mjs              print the current version (fails if files disagree)
//   node scripts/version.mjs 1.2.0        set the version everywhere
//   node scripts/version.mjs --check v1.2.0   CI: fail unless every file matches the tag

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const FILES = {
  pkg: join(ROOT, "package.json"),
  cargo: join(ROOT, "src-tauri", "Cargo.toml"),
  tauri: join(ROOT, "src-tauri", "tauri.conf.json"),
};
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
const CARGO_VERSION = /^(version\s*=\s*")([^"]+)(")/m;

function read() {
  const pkg = JSON.parse(readFileSync(FILES.pkg, "utf8")).version;
  const cargo = readFileSync(FILES.cargo, "utf8").match(CARGO_VERSION)?.[2];
  const tauri = JSON.parse(readFileSync(FILES.tauri, "utf8")).version;
  return { pkg, cargo, tauri };
}

function write(version) {
  const pkg = JSON.parse(readFileSync(FILES.pkg, "utf8"));
  pkg.version = version;
  writeFileSync(FILES.pkg, `${JSON.stringify(pkg, null, 2)}\n`);

  const cargo = readFileSync(FILES.cargo, "utf8");
  if (!CARGO_VERSION.test(cargo)) throw new Error("no version line in Cargo.toml");
  writeFileSync(FILES.cargo, cargo.replace(CARGO_VERSION, `$1${version}$3`));

  const tauri = JSON.parse(readFileSync(FILES.tauri, "utf8"));
  tauri.version = version;
  writeFileSync(FILES.tauri, `${JSON.stringify(tauri, null, 2)}\n`);
}

const [arg, value] = process.argv.slice(2);
const found = read();
const distinct = [...new Set(Object.values(found))];

if (arg === "--check") {
  const want = (value ?? "").replace(/^v/, "");
  const bad = Object.entries(found).filter(([, v]) => v !== want);
  if (!SEMVER.test(want) || bad.length > 0) {
    console.error(`Version mismatch for tag "${value}": ${JSON.stringify(found)}`);
    process.exit(1);
  }
  console.log(`All files at ${want}`);
} else if (arg) {
  if (!SEMVER.test(arg)) {
    console.error(`"${arg}" is not a semantic version (e.g. 1.2.0)`);
    process.exit(1);
  }
  write(arg);
  console.log(`Version set to ${arg}. Commit, then tag: git tag v${arg} && git push origin v${arg}`);
} else if (distinct.length !== 1) {
  console.error(`Versions disagree: ${JSON.stringify(found)}`);
  process.exit(1);
} else {
  console.log(distinct[0]);
}
