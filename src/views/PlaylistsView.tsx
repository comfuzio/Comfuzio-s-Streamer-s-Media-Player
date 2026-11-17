import { useCallback, useEffect, useState, type DragEvent, type FormEvent } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { ExternalLink } from "../components/ExternalLink";
import { Icon } from "../components/Icon";
import { api, artUrl, formatClock, onAppEvent } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { ExportOutcome, ImportOutcome, Playlist, PlaylistDetail, Track } from "../lib/types";

const SOURCE_LABEL = { youTube: "YouTube", spotify: "Spotify", m3u: "M3U file" } as const;

function originLabel(t: Track): string {
  switch (t.origin.kind) {
    case "local": return "Local";
    case "spotify": return t.origin.matchedVideoId ? "Spotify" : "Spotify (matched on play)";
    default: return "YouTube";
  }
}

function ImportPanel({ onImported }: { onImported: (o: ImportOutcome) => void }) {
  const { fail } = useStore();
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const run = (p: Promise<ImportOutcome>) => {
    setBusy(true);
    p.then((o) => { onImported(o); setUrl(""); }).catch(fail).finally(() => setBusy(false));
  };
  const submit = (e: FormEvent) => { e.preventDefault(); if (url.trim()) run(api.playlists.importUrl(url.trim())); };
  return (
    <section className="card">
      <h2>Import</h2>
      <form className="request" onSubmit={submit}>
        <input value={url} onChange={(e) => setUrl(e.target.value)} disabled={busy}
          placeholder="YouTube playlist link (…list=…) or Spotify playlist link" />
        <button className="primary" type="submit" disabled={busy || !url.trim()}>{busy ? "Importing…" : "Import link"}</button>
        <button type="button" disabled={busy} onClick={async () => {
          const file = await open({ multiple: false, title: "Import M3U playlist", filters: [{ name: "Playlist", extensions: ["m3u", "m3u8"] }] });
          if (typeof file === "string") run(api.playlists.importM3u(file));
        }}>M3U file…</button>
      </form>
      <p className="muted small">
        Imports are one-way copies that remember where they came from: use <strong>Pull</strong> later to refresh one.
        Spotify playlists need a connected Spotify account (Settings), and songs are matched to YouTube audio when they play.
      </p>
    </section>
  );
}

function Editor({ id, onDeleted }: { id: number; onDeleted: () => void }) {
  const { settings, patchSettings, notify, fail } = useStore();
  const [detail, setDetail] = useState<PlaylistDetail | null>(null);
  const [link, setLink] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const [over, setOver] = useState<number | null>(null);

  const load = useCallback(() => { api.playlists.get(id).then(setDetail).catch(fail); }, [id, fail]);
  useEffect(() => {
    load();
    const un = onAppEvent((e) => { if (e.type === "playlistsChanged") load(); });
    return () => { un.then((f) => f()); };
  }, [load]);

  if (!detail || !settings) return <section className="card"><p className="empty">Loading…</p></section>;
  const pl = detail.playlist;
  const isAutoDj = settings.autoDj.enabled && settings.autoDj.playlistId === pl.id;

  const task = <T,>(label: string, p: Promise<T>, done: (v: T) => void) => {
    setBusy(label);
    p.then(done).catch(fail).finally(() => setBusy(null));
  };
  const exported = (where: string) => (o: ExportOutcome) =>
    notify({ level: "info", message: `Exported ${o.exported} song${o.exported === 1 ? "" : "s"} to ${where}${o.skipped ? ` (${o.skipped} skipped)` : ""}${o.url ? `: ${o.url}` : ""}` });

  const onDrop = (e: DragEvent, index: number) => {
    e.preventDefault();
    const item = drag !== null ? detail.items[drag] : undefined;
    setDrag(null); setOver(null);
    if (item && drag !== index) api.playlists.moveItem(item.id, index).catch(fail);
  };

  return (
    <section className="card playlist-editor">
      <header className="section-head">
        <h2 className="pl-title" title="Click to rename" onClick={() => {
          const name = prompt("Rename playlist", pl.name);
          if (name && name !== pl.name) api.playlists.rename(pl.id, name).catch(fail);
        }}>{pl.name}</h2>
        <span className="muted">{pl.itemCount} songs · {formatClock(pl.totalDurationMs)}</span>
        {pl.source && <span className="pill">from {SOURCE_LABEL[pl.source.kind]}</span>}
      </header>

      <div className="row wrap">
        <button className="primary" disabled={pl.itemCount === 0} onClick={() => api.playlists.enqueue(pl.id, false)
          .then((n) => notify({ level: "info", message: `Queued ${n} songs` })).catch(fail)}><Icon name="play" size={16} /> Queue all</button>
        <button disabled={pl.itemCount === 0} onClick={() => api.playlists.enqueue(pl.id, true)
          .then((n) => notify({ level: "info", message: `Queued ${n} songs, shuffled` })).catch(fail)}>Shuffle into queue</button>
        <button className={isAutoDj ? "active" : ""} disabled={pl.itemCount === 0}
          onClick={() => patchSettings((s) => ({ ...s, autoDj: { ...s.autoDj, enabled: !isAutoDj, playlistId: pl.id } }))}>
          {isAutoDj ? "✓ Auto-DJ playlist" : "Use for Auto-DJ"}
        </button>
        {pl.source && (
          <button disabled={!!busy} onClick={() => task("pull", api.playlists.sync(pl.id), (o) =>
            notify({ level: "info", message: `Pulled ${o.imported} songs from ${SOURCE_LABEL[pl.source!.kind]}${o.skipped.length ? ` (${o.skipped.length} skipped)` : ""}` }))}>
            {busy === "pull" ? "Pulling…" : `Pull from ${SOURCE_LABEL[pl.source.kind]}`}
          </button>
        )}
        <span className="spacer-x" />
        <select className="picker" value="" disabled={!!busy || pl.itemCount === 0} onChange={async (e) => {
          const target = e.target.value;
          e.target.value = "";
          if (target === "m3u") {
            const path = await save({ defaultPath: `${pl.name}.m3u8`, filters: [{ name: "M3U8 playlist", extensions: ["m3u8"] }] });
            if (path) task("export", api.playlists.exportM3u(pl.id, path), exported("M3U"));
          } else if (target === "youtube") {
            const units = 50 + pl.itemCount * 50;
            if (confirm(`Creating a YouTube playlist costs about ${units} of your daily ${settings.youtubeChat.dailyQuota} YouTube API quota units (shared with the chat bot). Continue?`))
              task("export", api.playlists.exportYoutube(pl.id), exported("YouTube"));
          } else if (target === "spotify") {
            task("export", api.playlists.exportSpotify(pl.id), exported("Spotify"));
          }
        }}>
          <option value="">{busy === "export" ? "Exporting…" : "Export…"}</option>
          <option value="m3u">M3U8 file</option>
          <option value="youtube">YouTube playlist (private)</option>
          <option value="spotify">Spotify playlist (private)</option>
        </select>
        <button className="ghost danger" onClick={() => {
          if (confirm(`Delete the playlist "${pl.name}"? This can't be undone.`)) api.playlists.remove(pl.id).then(onDeleted).catch(fail);
        }}>Delete</button>
      </div>

      <form className="request" onSubmit={(e) => {
        e.preventDefault();
        if (link.trim()) task("add", api.playlists.addLink(pl.id, link.trim()), () => setLink(""));
      }}>
        <input value={link} onChange={(e) => setLink(e.target.value)} disabled={busy === "add"} placeholder="Add a YouTube / Spotify link or artist - song" />
        <button type="submit" disabled={busy === "add" || !link.trim()}>{busy === "add" ? "Adding…" : "Add"}</button>
        <button type="button" title="Append everything currently in the queue" onClick={() => api.playlists.addQueue(pl.id)
          .then((n) => notify({ level: "info", message: `Playlist now has ${n} songs` })).catch(fail)}>Add current queue</button>
      </form>

      {detail.items.length === 0 ? (
        <p className="empty">Empty playlist. Add songs from the Library tab, paste links above, or add the current queue.</p>
      ) : (
        <ol className="queue-list" onDragEnd={() => { setDrag(null); setOver(null); }}>
          {detail.items.map((item, i) => (
            <li key={item.id} draggable className={`queue-row${drag === i ? " dragging" : ""}${over === i && drag !== i ? " drop-target" : ""}`}
              onDragStart={(e) => { setDrag(i); e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", String(i)); }}
              onDragOver={(e) => { e.preventDefault(); if (over !== i) setOver(i); }}
              onDrop={(e) => onDrop(e, i)}>
              <span className="grip"><Icon name="grip" size={16} /></span>
              <span className="q-index">{i + 1}</span>
              {item.track.artworkUrl ? <img className="q-art" src={artUrl(item.track.artworkUrl, settings.overlayPort) ?? ""} alt="" loading="lazy" referrerPolicy="no-referrer" /> : <span className="q-art art-placeholder"><Icon name="music" size={18} /></span>}
              <span className="q-text">
                <span className="q-title">{item.track.title}</span>
                <span className="q-sub">{item.track.artist || "Unknown artist"} · {originLabel(item.track)}</span>
              </span>
              <span className="q-dur">{formatClock(item.track.durationMs)}</span>
              <span className="q-actions">
                {item.track.origin.kind !== "local" && <ExternalLink href={item.track.sourceUrl}><Icon name="external" size={16} /></ExternalLink>}
                <button className="icon-btn danger" title="Remove from playlist" onClick={() => api.playlists.removeItem(item.id).catch(fail)}><Icon name="trash" size={16} /></button>
              </span>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

export function PlaylistsView() {
  const { settings, patchSettings, notify, fail } = useStore();
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [active, setActive] = useState<number | null>(null);

  const load = useCallback(() => {
    api.playlists.list().then((list) => {
      setPlaylists(list);
      setActive((cur) => (cur !== null && list.some((p) => p.id === cur) ? cur : list[0]?.id ?? null));
    }).catch(fail);
  }, [fail]);

  useEffect(() => {
    load();
    const un = onAppEvent((e) => { if (e.type === "playlistsChanged") load(); });
    return () => { un.then((f) => f()); };
  }, [load]);

  if (!settings) return null;
  const autoDj = settings.autoDj;

  return (
    <div className="view">
      <section className="card">
        <header className="section-head">
          <h2>Auto-DJ</h2>
          <span className="muted small">Plays a playlist whenever nobody has requested anything. Requests always go first.</span>
        </header>
        <div className="row wrap">
          <label className="check">
            <input type="checkbox" checked={autoDj.enabled} disabled={autoDj.playlistId === null}
              onChange={(e) => patchSettings((s) => ({ ...s, autoDj: { ...s.autoDj, enabled: e.target.checked } }))} />
            Enabled
          </label>
          <select className="picker" value={autoDj.playlistId ?? ""}
            onChange={(e) => patchSettings((s) => ({ ...s, autoDj: { ...s.autoDj, playlistId: e.target.value ? Number(e.target.value) : null } }))}>
            <option value="">Choose a playlist…</option>
            {playlists.map((p) => <option key={p.id} value={p.id}>{p.name} ({p.itemCount})</option>)}
          </select>
          <label className="check">
            <input type="checkbox" checked={autoDj.shuffle}
              onChange={(e) => patchSettings((s) => ({ ...s, autoDj: { ...s.autoDj, shuffle: e.target.checked } }))} />
            Shuffle
          </label>
        </div>
      </section>

      <ImportPanel onImported={(o) => {
        notify({ level: "info", message: `Imported "${o.playlist.name}": ${o.imported} songs${o.skipped.length ? `, ${o.skipped.length} skipped` : ""}` });
        if (o.skipped.length) console.warn("Skipped during import:", o.skipped);
        setActive(o.playlist.id);
      }} />

      <div className="playlists-layout">
        <section className="card playlist-list">
          <header className="section-head">
            <h2>Playlists</h2>
            <button className="small" onClick={() => {
              const name = prompt("New playlist name");
              if (name) api.playlists.create(name).then((p) => setActive(p.id)).catch(fail);
            }}>+ New</button>
          </header>
          {playlists.length === 0 ? <p className="empty">No playlists yet.</p> : (
            <ul>
              {playlists.map((p) => (
                <li key={p.id}>
                  <button className={`nav-item${active === p.id ? " active" : ""}`} onClick={() => setActive(p.id)}>
                    <span className="pl-name">{p.name}</span>
                    <span className="muted small">{p.itemCount}{autoDj.enabled && autoDj.playlistId === p.id ? " · DJ" : ""}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
        {active !== null ? <Editor key={active} id={active} onDeleted={() => setActive(null)} /> : (
          <section className="card"><p className="empty">Create or import a playlist to get started.</p></section>
        )}
      </div>
    </div>
  );
}
