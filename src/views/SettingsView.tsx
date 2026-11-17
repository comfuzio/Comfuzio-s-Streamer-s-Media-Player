import { useEffect, useState } from "react";
import { ExternalLink } from "../components/ExternalLink";
import { api, formatClock } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { OutputDevice, Settings, SpotifyStatus } from "../lib/types";

/** Text field that saves on blur / Enter, so typing doesn't save every keystroke. */
function TextSetting({ label, value, onSave, placeholder, type = "text", hint }: {
  label: string; value: string | null; onSave: (v: string | null) => void; placeholder?: string; type?: string; hint?: string;
}) {
  const [draft, setDraft] = useState(value ?? "");
  useEffect(() => setDraft(value ?? ""), [value]);
  const commit = () => {
    const v = draft.trim() || null;
    if (v !== (value ?? null)) onSave(v);
  };
  return (
    <label>
      {label}
      <input type={type} value={draft} placeholder={placeholder} onChange={(e) => setDraft(e.target.value)}
        onBlur={commit} onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }} />
      {hint && <span className="muted small">{hint}</span>}
    </label>
  );
}

function NumberSetting({ label, value, min, max, onSave, suffix }: {
  label: string; value: number; min: number; max: number; onSave: (v: number) => void; suffix?: string;
}) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  return (
    <label>
      {label}
      <span className="input-suffix">
        <input type="number" min={min} max={max} value={draft} onChange={(e) => setDraft(e.target.value)}
          onBlur={() => {
            const n = Math.round(Number(draft));
            if (Number.isFinite(n) && n !== value) onSave(Math.min(max, Math.max(min, n)));
            else setDraft(String(value));
          }} />
        {suffix && <span className="muted">{suffix}</span>}
      </span>
    </label>
  );
}

export function SettingsView() {
  const { settings, patchSettings, notify, fail } = useStore();
  const [devices, setDevices] = useState<OutputDevice[]>([]);
  const [spotify, setSpotify] = useState<SpotifyStatus | null>(null);
  const [ytdlp, setYtdlp] = useState<string>("…");
  const [maxLength, setMaxLength] = useState("");

  useEffect(() => {
    api.audioDevices().then(setDevices).catch(fail);
    api.spotify.status().then(setSpotify).catch(fail);
    api.ytDlpVersion().then(setYtdlp).catch((e) => setYtdlp(`unavailable (${String((e as { message?: string }).message ?? e)})`));
  }, [fail]);

  const applyMaxLength = () => {
    if (!maxLength.trim()) return;
    api.settings.setMaxLength(maxLength).then((secs) => {
      setMaxLength("");
      notify({ level: "info", message: `Max song length set to ${formatClock(secs * 1000)}` });
    }).catch(fail);
  };

  if (!settings) return null;
  const patch = (fn: (s: Settings) => Settings) => patchSettings(fn);
  const ns = settings.commandNamespace;

  return (
    <div className="view settings-view">
      <section className="card">
        <h2>Requests</h2>
        <div className="form-grid">
          <TextSetting label="Chat command namespace" value={ns}
            hint={`Commands become !${ns}sr, !${ns}skip, !${ns}song …`}
            onSave={(v) => patch((s) => ({ ...s, commandNamespace: v ?? "mb" }))} />
          <label>
            Max song length · now {formatClock(settings.maxLengthSecs * 1000)}
            <span className="row">
              <input value={maxLength} placeholder="30s, 5m, 1h" onChange={(e) => setMaxLength(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") applyMaxLength(); }} />
              <button disabled={!maxLength.trim()} onClick={applyMaxLength}>Set</button>
            </span>
            <span className="muted small">Between 30 seconds and 1 hour. Mods and the streamer are exempt.</span>
          </label>
          <NumberSetting label="Songs per viewer in queue" value={settings.queue.maxPerUser} min={1} max={50}
            onSave={(v) => patch((s) => ({ ...s, queue: { ...s.queue, maxPerUser: v } }))} />
          <NumberSetting label="Max queue size" value={settings.queue.maxSize} min={1} max={5000}
            onSave={(v) => patch((s) => ({ ...s, queue: { ...s.queue, maxSize: v } }))} />
          <NumberSetting label="Request cooldown per viewer" value={settings.chat.requestCooldownSecs} min={0} max={600} suffix="s"
            onSave={(v) => patch((s) => ({ ...s, chat: { ...s.chat, requestCooldownSecs: v } }))} />
          <NumberSetting label="!song / !queue cooldown" value={settings.chat.infoCooldownSecs} min={0} max={600} suffix="s"
            onSave={(v) => patch((s) => ({ ...s, chat: { ...s.chat, infoCooldownSecs: v } }))} />
          <label className="check">
            <input type="checkbox" checked={settings.queue.allowDuplicates}
              onChange={(e) => patch((s) => ({ ...s, queue: { ...s.queue, allowDuplicates: e.target.checked } }))} />
            Allow the same song in the queue twice
          </label>
        </div>
      </section>

      <section className="card">
        <h2>App</h2>
        <div className="form-grid">
          <label className="check">
            <input type="checkbox" checked={settings.closeToTray}
              onChange={(e) => patch((s) => ({ ...s, closeToTray: e.target.checked }))} />
            Keep running in the tray when the window is closed
          </label>
          <label className="check">
            <input type="checkbox" checked={settings.startInTray}
              onChange={(e) => patch((s) => ({ ...s, startInTray: e.target.checked }))} />
            Start in the tray (do not open this window on launch)
          </label>
        </div>
        <p className="muted small">
          Closing the window fully unloads the dashboard (about 170 MB) while music, chat bot and overlay keep running.
          Reopen it from the tray icon, or by launching the app again.
        </p>
      </section>

      <section className="card">
        <h2>Audio</h2>
        <div className="form-grid">
          <label>
            Output device
            <select value={settings.outputDevice ?? ""} onChange={(e) => patch((s) => ({ ...s, outputDevice: e.target.value || null }))}>
              <option value="">System default</option>
              {devices.map((d) => <option key={d.name} value={d.name}>{d.name}{d.isDefault ? " (default)" : ""}</option>)}
            </select>
            <span className="muted small">In OBS, capture this app with “Application Audio Capture” to keep game and alert sounds out.</span>
          </label>
          <NumberSetting label="Audio cache size" value={settings.cacheMaxMib} min={64} max={32768} suffix="MiB"
            onSave={(v) => patch((s) => ({ ...s, cacheMaxMib: v }))} />
        </div>
      </section>

      <section className="card">
        <h2>YouTube playback</h2>
        <p className="muted small">yt-dlp {ytdlp}</p>
        <div className="form-grid">
          <TextSetting label="Cookies file (cookies.txt)" value={settings.youtube.cookiesFile} placeholder="C:\path\to\cookies.txt"
            hint="Optional. A Premium account's cookies unlock 256 kbps audio."
            onSave={(v) => patch((s) => ({ ...s, youtube: { ...s.youtube, cookiesFile: v } }))} />
          <TextSetting label="…or read cookies from browser" value={settings.youtube.cookiesFromBrowser} placeholder="firefox, chrome, edge"
            onSave={(v) => patch((s) => ({ ...s, youtube: { ...s.youtube, cookiesFromBrowser: v } }))} />
          <TextSetting label="yt-dlp JS runtimes" value={settings.youtube.jsRuntimes} placeholder="deno"
            hint="Only needed if YouTube stops returning audio formats."
            onSave={(v) => patch((s) => ({ ...s, youtube: { ...s.youtube, jsRuntimes: v } }))} />
          <TextSetting label="Custom yt-dlp path" value={settings.youtube.ytDlpPath} placeholder="bundled"
            onSave={(v) => patch((s) => ({ ...s, youtube: { ...s.youtube, ytDlpPath: v } }))} />
        </div>
      </section>

      <section className="card">
        <h2>Spotify</h2>
        <p className="muted small">
          Spotify links always work. Connecting an account adds cleaner search results. Create an app at{" "}
          <ExternalLink href="https://developer.spotify.com/dashboard">developer.spotify.com</ExternalLink> and add the redirect URI{" "}
          <code>{spotify?.redirectUri}</code>.
        </p>
        <div className="form-grid">
          <TextSetting label="Client ID" value={settings.spotify.clientId}
            onSave={(v) => patch((s) => ({ ...s, spotify: { ...s.spotify, clientId: v } }))} />
          <TextSetting label="Market (country code)" value={settings.spotify.market} placeholder="US"
            onSave={(v) => patch((s) => ({ ...s, spotify: { ...s.spotify, market: v } }))} />
          <label>
            Search text requests on
            <select value={settings.searchProvider} onChange={(e) => patch((s) => ({ ...s, searchProvider: e.target.value as Settings["searchProvider"] }))}>
              <option value="youTube">YouTube</option>
              <option value="spotify">Spotify (needs a connected account)</option>
            </select>
          </label>
          <div className="row align-end">
            {spotify?.connected ? (
              <button onClick={() => api.spotify.logout().then(setSpotify).catch(fail)}>Disconnect Spotify</button>
            ) : (
              <button className="primary" disabled={!settings.spotify.clientId} onClick={() => api.spotify.login().then(setSpotify).catch(fail)}>Connect Spotify</button>
            )}
            <span className="muted small">{spotify?.connected ? "Connected" : "Not connected"}</span>
          </div>
        </div>
      </section>

      <section className="card">
        <h2>Overlay server</h2>
        <div className="form-grid">
          <NumberSetting label="Port" value={settings.overlayPort} min={1024} max={65535}
            onSave={(v) => patch((s) => ({ ...s, overlayPort: v }))} />
        </div>
      </section>
    </div>
  );
}
