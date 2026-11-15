#!/usr/bin/env node
// Downloads the yt-dlp sidecar binaries Tauri bundles via `bundle.externalBin`,
// verifying each file against the release's SHA2-256SUMS manifest.
//
// Usage:
//   node scripts/fetch-sidecars.mjs                 # host target only
//   node scripts/fetch-sidecars.mjs --target <triple>
//   node scripts/fetch-sidecars.mjs --all           # every supported target
//   node scripts/fetch-sidecars.mjs --force         # re-download even if present
//   YTDLP_VERSION=2025.10.22 node scripts/...       # pin a release (default: latest)

import { createHash } from "node:crypto";
import { chmod, mkdir, rename, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { arch, platform } from "node:process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = join(ROOT, "src-tauri", "binaries");

/** Supported target triple -> yt-dlp release asset name. */
const ASSETS = {
  "x86_64-pc-windows-msvc": "yt-dlp.exe",
  "aarch64-pc-windows-msvc": "yt-dlp_arm64.exe",
  "aarch64-apple-darwin": "yt-dlp_macos",
  "x86_64-unknown-linux-gnu": "yt-dlp_linux",
  "aarch64-unknown-linux-gnu": "yt-dlp_linux_aarch64",
};

function hostTriple() {
  const key = `${platform}-${arch}`;
  const map = {
    "win32-x64": "x86_64-pc-windows-msvc",
    "win32-arm64": "aarch64-pc-windows-msvc",
    "darwin-arm64": "aarch64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
  };
  const triple = map[key];
  if (!triple) throw new Error(`Unsupported host ${key}. Supported: ${Object.keys(map).join(", ")}`);
  return triple;
}

function parseArgs(argv) {
  const args = { targets: [], force: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--all") args.targets = Object.keys(ASSETS);
    else if (a === "--force") args.force = true;
    else if (a === "--target") {
      const t = argv[++i];
      if (!t || !(t in ASSETS)) throw new Error(`--target must be one of: ${Object.keys(ASSETS).join(", ")}`);
      args.targets.push(t);
    } else throw new Error(`Unknown argument: ${a}`);
  }
  if (args.targets.length === 0) args.targets = [hostTriple()];
  return args;
}

async function fetchOk(url) {
  const res = await fetch(url, { headers: { "User-Agent": "comfuzio-player-build" }, redirect: "follow" });
  if (!res.ok) throw new Error(`GET ${url} -> HTTP ${res.status}`);
  return res;
}

function outPath(triple) {
  return join(OUT_DIR, `yt-dlp-${triple}${triple.includes("windows") ? ".exe" : ""}`);
}

async function main() {
  const { targets, force } = parseArgs(process.argv.slice(2));
  const version = process.env.YTDLP_VERSION;
  const base = version
    ? `https://github.com/yt-dlp/yt-dlp/releases/download/${version}`
    : "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

  const pending = targets.filter((t) => force || !existsSync(outPath(t)));
  if (pending.length === 0) {
    console.log("yt-dlp sidecars already present:", targets.join(", "));
    return;
  }

  const sums = new Map(
    (await (await fetchOk(`${base}/SHA2-256SUMS`)).text())
      .split("\n")
      .map((line) => line.trim().split(/\s+/))
      .filter((parts) => parts.length === 2)
      .map(([hash, name]) => [name, hash.toLowerCase()]),
  );

  await mkdir(OUT_DIR, { recursive: true });
  for (const triple of pending) {
    const asset = ASSETS[triple];
    const expected = sums.get(asset);
    if (!expected) throw new Error(`Release has no checksum for ${asset}`);

    console.log(`Downloading ${asset} for ${triple}...`);
    const bytes = Buffer.from(await (await fetchOk(`${base}/${asset}`)).arrayBuffer());
    const actual = createHash("sha256").update(bytes).digest("hex");
    if (actual !== expected) throw new Error(`Checksum mismatch for ${asset}: expected ${expected}, got ${actual}`);

    const dest = outPath(triple);
    const tmp = `${dest}.download`;
    await writeFile(tmp, bytes);
    if (!triple.includes("windows")) await chmod(tmp, 0o755);
    await rename(tmp, dest);
    console.log(`  -> ${dest} (${(bytes.length / 1048576).toFixed(1)} MiB, sha256 ok)`);
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
