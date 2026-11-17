import { useEffect, useState } from "react";
import { getTauriVersion, getVersion } from "@tauri-apps/api/app";
import { ExternalLink } from "../components/ExternalLink";
import { useStore } from "../lib/store";
import { checkForUpdate, installUpdate, useUpdateState } from "../lib/updates";

export const REPO_URL = "https://github.com/comfuzio/Comfuzio-s-Streamer-s-Media-Player";

function mib(bytes: number): string {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

export function AboutView() {
  const { settings, patchSettings } = useStore();
  const update = useUpdateState();
  const [version, setVersion] = useState("…");
  const [tauri, setTauri] = useState("…");

  useEffect(() => {
    getVersion().then(setVersion).catch(() => setVersion("unknown"));
    getTauriVersion().then(setTauri).catch(() => setTauri("unknown"));
  }, []);

  const busy = update.phase === "checking" || update.phase === "downloading" || update.phase === "installing";
  const pct = update.progress?.total ? Math.min(100, (update.progress.done / update.progress.total) * 100) : null;

  return (
    <div className="view about-view">
      <section className="card about-hero">
        <img src="/app-icon.svg" alt="" width={72} height={72} />
        <div>
          <h2>Comfuzio Streamer's Media Player</h2>
          <p className="muted">Music player and chat song-request bot for live streamers.</p>
          <p className="small muted">Version <strong>{version}</strong> · Tauri {tauri}</p>
          <div className="row">
            <ExternalLink href={REPO_URL}><span className="link-btn">GitHub page</span></ExternalLink>
            <ExternalLink href={`${REPO_URL}/releases`}><span className="link-btn">Releases</span></ExternalLink>
            <ExternalLink href={`${REPO_URL}/issues`}><span className="link-btn">Report a problem</span></ExternalLink>
          </div>
        </div>
      </section>

      <section className="card">
        <header className="section-head">
          <h2>Updates</h2>
          <button className="primary" disabled={busy} onClick={() => checkForUpdate()}>
            {update.phase === "checking" ? "Checking…" : "Check for updates"}
          </button>
        </header>

        {update.phase === "available" && update.update && (
          <div className="update-box">
            <p><strong>Version {update.update.version}</strong> is available{update.update.date ? ` (released ${new Date(update.update.date).toLocaleDateString()})` : ""}.</p>
            {update.update.body && <pre className="release-notes">{update.update.body}</pre>}
            <button className="primary" onClick={() => installUpdate()}>Download &amp; install</button>
            <p className="muted small">The app restarts to finish. Music stops briefly; your queue is kept.</p>
          </div>
        )}
        {(update.phase === "downloading" || update.phase === "installing") && (
          <div className="update-box">
            <p>{update.phase === "installing" ? "Installing…" : `Downloading… ${update.progress ? mib(update.progress.done) : ""}${update.progress?.total ? ` of ${mib(update.progress.total)}` : ""}`}</p>
            <div className="bar"><div className="fill" style={{ width: `${pct ?? 100}%` }} /></div>
          </div>
        )}
        {update.message && <p className={update.phase === "error" ? "error-text" : "muted"}>{update.message}</p>}
        {update.checkedAtMs && <p className="muted small">Last checked {new Date(update.checkedAtMs).toLocaleTimeString()}.</p>}

        {settings && (
          <label className="check">
            <input type="checkbox" checked={settings.autoCheckUpdates}
              onChange={(e) => patchSettings((s) => ({ ...s, autoCheckUpdates: e.target.checked }))} />
            Check for updates automatically when the app starts
          </label>
        )}
        <p className="muted small">Updates are downloaded from GitHub and verified against the app's signing key before installing.</p>
      </section>

      <section className="card">
        <h2>License</h2>
        <p className="muted small">
          Copyright © 2026 Comfuzio. All rights reserved. Official release builds may be downloaded and used.
          Bundled third-party components (including yt-dlp and the open-source libraries this app is built with) keep their own licenses.
        </p>
      </section>
    </div>
  );
}
