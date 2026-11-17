# Comfuzio Streamer's Media Player

A desktop music player and chat song-request bot for live streamers. Built with Tauri v2: a Rust backend and a React + TypeScript + Vite frontend.

## Why Tauri v2

- **Clean OBS capture.** Audio is decoded and played by the main Rust process (cpal → WASAPI / CoreAudio / ALSA→PipeWire). It never goes through the webview, which on Windows runs in separate `msedgewebview2.exe` processes. OBS "Application Audio Capture" can therefore target `comfuzio-player.exe` and nothing else.
- **Embedded servers.** The overlay HTTP/WebSocket server and the chat connectors run on the same Tokio runtime as the player. They need no extra runtime or IPC hop.
- **Small footprint.** The app uses the system webview, ships as a single native binary plus the `yt-dlp` sidecar, and has no bundled Chromium or Node.

## Layout

```
├─ package.json / vite.config.ts / tsconfig.json / index.html
├─ scripts/fetch-sidecars.mjs     # downloads + SHA-256-verifies yt-dlp per target
├─ src/                           # React dashboard
│  ├─ App.tsx, main.tsx, styles.css
│  ├─ views/                      # Player, Library, Playlists, Overlay, Chat bot, Blocklist, Settings
│  └─ lib/ipc.ts, lib/types.ts    # typed IPC bindings mirroring the Rust types
└─ src-tauri/
   ├─ Cargo.toml, build.rs        # build.rs rejects unsupported target triples
   ├─ tauri.conf.json             # + tauri.{windows,macos,linux}.conf.json bundle targets
   ├─ capabilities/default.json
   ├─ binaries/                   # yt-dlp-<target-triple>[.exe] (fetched, git-ignored)
   └─ src/
      ├─ lib.rs, main.rs, state.rs, commands.rs, events.rs, error.rs, model.rs, tray.rs
      ├─ util/        duration parsing (30s/5m/1h, clamp 30s..1h), text cleanup
      ├─ persistence/ SQLite (WAL, versioned migrations): settings, blocklist, queue, history, kv
      ├─ resolver/    input parsing, yt-dlp wrapper, Spotify PKCE client, YouTube matcher
      ├─ audio/       engine (cpal thread), decoder (Symphonia + rubato), cache, devices, OS identity
      ├─ library/     local music: folder scan, tags + cover art (lofty), search
      ├─ playlists/   playlists, M3U, import (YouTube/Spotify/M3U) and export
      ├─ chat/        Twitch EventSub + YouTube Live Chat bot
      ├─ overlay/     OBS overlay HTTP/WebSocket server
      └─ player/      PlaybackController: queue, Auto-DJ, transport state machine
```

## Supported targets

| OS      | Targets                                          | Bundle   |
|---------|--------------------------------------------------|----------|
| Windows | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | NSIS     |
| macOS   | `aarch64-apple-darwin` (macOS 13+)                | app, dmg |
| Linux   | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | AppImage |

Any other target triple fails in `build.rs`.

## Development

Prerequisites: Rust stable, Node 22+, and the platform's Tauri prerequisites. On Linux these include `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev` (tray icon) and `libasound2-dev`.

```bash
npm install
npm run app:dev          # fetches the yt-dlp sidecar for this machine, then `tauri dev`
cd src-tauri && cargo test
```

See [Building & releasing](#building--releasing) for release builds.

## Features

### Tray mode (low memory while streaming)

Closing the window keeps the app running in the system tray. Music, the chat bot and the overlay keep working, and the dashboard's webview is destroyed rather than hidden. Measured on Windows, memory drops from about 200 MB with the window open to about 9 MB in the tray. Click the tray icon or launch the app again to reopen the dashboard; a second launch never starts a second copy. The tray menu also has Play / Pause, Skip and Quit. Both behaviors can be changed in **Settings → App**, along with "start in the tray".

### Local music library

Add music folders in **Library** and click **Scan**. The app reads MP3, FLAC, M4A/AAC, ALAC, OGG Vorbis and WAV files, with their tags, embedded cover art or a `cover.jpg` / `folder.jpg` next to the files. Untagged files fall back to `Artist - Title` file names. Rescans only re-read new or changed files. Local songs play straight from disk with no download, and their cover art also appears in the OBS overlay.

The option **"Let chat requests play matching songs from this library first"** makes `!<ns>sr artist - song` check your library before searching online. It's off by default.

### Playlists and Auto-DJ

**Playlists** can mix local, YouTube and Spotify songs. You can reorder them by dragging, queue them whole or shuffled, or add the current queue to one.

**Auto-DJ** plays a chosen playlist, in order or shuffled, whenever nobody has requested anything. Viewer requests always play next, and Auto-DJ picks up where it left off after them. Blocked songs are skipped, the overlay shows "from <playlist>" instead of "requested by", and Stop never turns Auto-DJ songs into requests.

### Import and export

| | Import (one-way copy, re-pull with **Pull**) | Export |
|---|---|---|
| **M3U / M3U8** | Local paths (relative or absolute) and YouTube/Spotify links | Writes `#EXTINF` + paths/links |
| **YouTube** | Playlist links (`…list=…`), YouTube Music playlists and mixes. No account needed for public or unlisted playlists | Creates a private playlist. Needs the Google sign-in from the Chat bot tab and costs about 50 quota units per song |
| **Spotify** | Your own or collaborative playlists. Needs a connected account; songs are matched to YouTube audio when they first play | Creates a private playlist. Needs a connected account; Spotify may refuse playlist writes for Development Mode apps |

Imports are snapshots, not live sync: **Pull** replaces a playlist's songs with the current state of its source. Accounts connected before this version need to **reconnect Spotify** once so it grants the playlist permissions.

## Spotify setup

1. Create an app at <https://developer.spotify.com/dashboard>.
2. Add the redirect URI `http://127.0.0.1:3848/callback`.
3. Paste the app's Client ID into Settings and click **Connect Spotify**. The app uses PKCE, so no client secret is needed.

Spotify links also work without an account: metadata comes from the public track page. Playback of Spotify tracks is always matched to a YouTube audio stream. That way every viewer request plays, and the audio stays inside the app's own capturable audio session.

## Chat bot setup

Commands use `!<namespace><command>`. The namespace defaults to `mb` and can be changed in Settings.

| Who | Commands |
|---|---|
| Everyone | `sr <link or artist - song>`, `song`, `queue`, `wrongsong` |
| Broadcaster & mods | `start`, `stop`, `skip`, `vol <0-100>`, `volup`, `voldown`, `clear`, `block [link]`, `unblock <link>`, `maxlength <30s/5m/1h>`, and `sr play`/`sr pause`/`sr stop` |

Moderator-only commands from viewers are ignored silently. Viewers have a per-user request cooldown and one request resolving at a time. `song` and `queue` replies have a per-channel cooldown. Mods and the broadcaster are never rate limited.

### Twitch

1. At <https://dev.twitch.tv/console/apps>, register an app with **Client Type: Public**. The OAuth redirect URL can be `http://localhost` (it isn't used).
2. Paste the Client ID into Settings, enable Twitch, and click **Sign in with Twitch**. Enter the code shown on the activation page (device-code flow, no secret needed).
3. Optionally set **Channel** if the account you signed in with is a separate bot account.

Chat arrives over EventSub WebSocket (`channel.chat.message`). Replies are sent with Helix *Send Chat Message* and threaded under the request, limited to 20 per 30 s. When `stream.offline` fires, playback stops if that option is on.

### YouTube

1. In Google Cloud Console, enable **YouTube Data API v3** and create an OAuth client of type **Desktop app**.
2. Paste the client ID and secret into Settings, enable YouTube, and click **Sign in with Google**.
3. The bot attaches automatically when your channel has an active broadcast. It checks every 90 s while you're offline.

Quota: the default project quota is 10,000 units per day. Reading chat costs 5 units per poll. The app follows YouTube's requested poll interval and never polls faster than every 6 s, which is about 3,000 units per hour of streaming. Chat replies cost 20 units each, so they're off by default. The app tracks usage per Pacific-time day and pauses before it runs past the budget. To stream longer, request a quota increase or raise `minPollSecs`.

## OBS overlay

While the app runs, it serves a Now Playing overlay at `http://127.0.0.1:3847/overlay`. You can change the port in Settings.

1. In OBS, go to **Sources → + → Browser** and paste the URL. Use the width and height shown in the Overlay tab (default 552 × 200).
2. Customize it in the app's **Overlay** tab: theme (Minimal, Neon, Glass, Vinyl), colors and opacity, font, scale, width, corner radius, and which elements show. Changes reach OBS instantly over WebSocket, with no refresh.
3. Optional URL parameters:
   - `?theme=vinyl` uses a different theme per scene.
   - `?demo=1` shows a sample track while idle.

Other tools can read the current track from `GET /api/now-playing` (JSON). The server only listens on 127.0.0.1, and it rejects requests with foreign Host or Origin headers.

For audio, add an **Application Audio Capture** source in OBS (Windows) and pick `comfuzio-player.exe`. On macOS use **macOS Audio Capture**, and on Linux the PipeWire application capture, picking "Comfuzio Streamer Player".

## Building & releasing

### Local builds

Each command fetches the matching yt-dlp sidecar, then builds that target's installer into `src-tauri/target/<triple>/release/bundle/`.

| Target | Command | Output | Build on |
|---|---|---|---|
| Windows x64 | `npm run build:win-x64` | `nsis/*_x64-setup.exe` | Windows |
| Windows arm64 | `npm run build:win-arm64` | `nsis/*_arm64-setup.exe` | Windows (needs Rust target `aarch64-pc-windows-msvc` and VS "C++ ARM64 build tools" + LLVM) |
| macOS Apple Silicon | `npm run build:mac-arm64` | `dmg/*_aarch64.dmg`, `macos/*.app` | macOS on Apple Silicon |
| Linux x64 | `npm run build:linux-x64` | `appimage/*_amd64.AppImage` | Linux x64 |
| Linux x64 (x86-64-v3) | `npm run build:linux-x64-v3` | `appimage/*_amd64.AppImage` (AVX2 build) | Linux x64 |
| Linux arm64 | `npm run build:linux-arm64` | `appimage/*_aarch64.AppImage` | Linux arm64 |

Any other target, such as 32-bit Windows or Intel macOS, fails in `build.rs` before anything compiles. Run `npm run check` to typecheck, run clippy with warnings treated as errors, and run the tests.

### Releases (GitHub Actions)

- `.github/workflows/ci.yml` runs the checks on Windows, macOS and Linux for every push and pull request.
- `.github/workflows/release.yml` builds all five targets in parallel and attaches them to a **draft** GitHub Release.

To cut a release:

```bash
npm run version:set 0.2.0          # updates package.json, Cargo.toml, tauri.conf.json
git commit -am "Release 0.2.0"
git tag v0.2.0 && git push origin main v0.2.0
```

The workflow refuses a tag that doesn't match the version in all three files. Review the draft release on GitHub, then publish it.

Notes:

- **Linux baseline:** the AppImages are built on Ubuntu 26.04 (`ubuntu-26.04` / `ubuntu-26.04-arm`), so they need glibc 2.43 or newer: Ubuntu 26.04, Fedora 44, current Arch and openSUSE Tumbleweed. Older distros such as Ubuntu 24.04 and Debian 13 can't run them.
- **x86-64-v3:** the release ships a second x64 AppImage compiled for x86-64-v3 (AVX2, BMI1/2, FMA, MOVBE: Intel Haswell 2013+ and AMD Excavator/Zen+), matching Ubuntu's `amd64v3` variant. CI checks that the binary really contains AVX code. On CPUs without AVX2, such as some Pentium/Celeron models sold until about 2021, it crashes with "illegal instruction", so the baseline `amd64` build stays available.
- **Linux arm64:** the `ubuntu-26.04-arm` runner is free for public repositories. Private repositories need a plan that includes arm64 Linux runners.
- **macOS signing:** without signing secrets, builds are ad-hoc signed. To produce a signed, notarized `.dmg`, add the `APPLE_*` secrets listed at the top of `release.yml`.
- **Windows signing:** builds are unsigned unless you add a code-signing certificate, so SmartScreen shows "unknown publisher" until the app builds reputation.

## License

Copyright (c) 2026 Comfuzio. All rights reserved. The source is published for viewing only; see [LICENSE](LICENSE). Official release builds may be downloaded and used. Bundled third-party components such as yt-dlp keep their own licenses.
