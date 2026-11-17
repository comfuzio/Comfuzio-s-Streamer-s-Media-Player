import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { Icon } from "../components/Icon";
import { api, artUrl, formatClock, onAppEvent } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { LibraryStatus, LibraryTrack, Playlist } from "../lib/types";

export function LibraryView() {
  const { settings, patchSettings, notify, fail } = useStore();
  const [status, setStatus] = useState<LibraryStatus | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<LibraryTrack[]>([]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());

  const refresh = useCallback(() => {
    api.library.status().then(setStatus).catch(fail);
  }, [fail]);

  useEffect(() => {
    refresh();
    api.playlists.list().then(setPlaylists).catch(fail);
    const un = onAppEvent((e) => {
      if (e.type === "libraryChanged") refresh();
      if (e.type === "playlistsChanged") api.playlists.list().then(setPlaylists).catch(fail);
    });
    return () => { un.then((f) => f()); };
  }, [refresh, fail]);

  // Debounced search; an empty query lists the start of the library.
  useEffect(() => {
    const t = window.setTimeout(() => {
      api.library.search(query).then((r) => { setResults(r); setSelected(new Set()); }).catch(fail);
    }, 200);
    return () => window.clearTimeout(t);
  }, [query, status?.total, fail]);

  if (!settings) return null;
  const folders = settings.library.folders;

  const addFolder = async () => {
    const dir = await open({ directory: true, multiple: false, title: "Add a music folder" });
    if (typeof dir === "string" && !folders.includes(dir)) {
      patchSettings((s) => ({ ...s, library: { ...s.library, folders: [...s.library.folders, dir] } }));
      notify({ level: "info", message: "Folder added. Click Scan to index it." });
    }
  };
  const removeFolder = (dir: string) =>
    patchSettings((s) => ({ ...s, library: { ...s.library, folders: s.library.folders.filter((f) => f !== dir) } }));

  const ids = selected.size > 0 ? [...selected] : [];
  const toggle = (id: number) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const enqueue = (list: number[], next: boolean) =>
    api.library.enqueue(list, next)
      .then((n) => notify({ level: "info", message: `${next ? "Playing next" : "Queued"}: ${n} song${n === 1 ? "" : "s"}` }))
      .catch(fail);

  const addToPlaylist = (playlistId: number, list: number[]) =>
    api.playlists.addLibrary(playlistId, list)
      .then(() => notify({ level: "info", message: `Added ${list.length} song${list.length === 1 ? "" : "s"} to the playlist` }))
      .catch(fail);

  return (
    <div className="view">
      <section className="card">
        <header className="section-head">
          <h2>Music folders</h2>
          <span className="muted">{status ? `${status.total} tracks` : ""}</span>
          <span className="row">
            <button onClick={addFolder}>Add folder</button>
            <button className="primary" disabled={status?.scanning || folders.length === 0}
              onClick={() => { setStatus((s) => s && { ...s, scanning: true }); api.library.scan().catch(fail).finally(refresh); }}>
              {status?.scanning ? "Scanning…" : "Scan"}
            </button>
          </span>
        </header>
        {folders.length === 0 ? (
          <p className="empty">Add the folders that hold your music (MP3, FLAC, M4A/AAC, ALAC, OGG Vorbis, WAV).</p>
        ) : (
          <ul className="folder-list">
            {folders.map((f) => (
              <li key={f}><code>{f}</code><button className="ghost danger small" onClick={() => removeFolder(f)}>Remove</button></li>
            ))}
          </ul>
        )}
        <label className="check">
          <input type="checkbox" checked={settings.library.matchRequests}
            onChange={(e) => patchSettings((s) => ({ ...s, library: { ...s.library, matchRequests: e.target.checked } }))} />
          Let chat requests (<code>!{settings.commandNamespace}sr artist - song</code>) play matching songs from this library first
        </label>
      </section>

      <section className="card">
        <header className="section-head">
          <h2>Tracks</h2>
          <input className="search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search artist, title, album…" />
        </header>
        {ids.length > 0 && (
          <div className="bulk-bar">
            <span>{ids.length} selected</span>
            <button onClick={() => enqueue(ids, false)}>Add to queue</button>
            <button onClick={() => enqueue(ids, true)}>Play next</button>
            <PlaylistPicker playlists={playlists} onPick={(pid) => addToPlaylist(pid, ids)} />
            <button className="ghost" onClick={() => setSelected(new Set())}>Clear</button>
          </div>
        )}
        {results.length === 0 ? (
          <p className="empty">{status?.total ? "No matches." : "Your library is empty. Add a folder and scan it."}</p>
        ) : (
          <ul className="queue-list">
            {results.map((t) => (
              <li key={t.id} className={`queue-row lib-row${selected.has(t.id) ? " selected" : ""}`}>
                <input type="checkbox" checked={selected.has(t.id)} onChange={() => toggle(t.id)} aria-label={`Select ${t.title}`} />
                {t.hasArt ? <img className="q-art" src={artUrl(`/art/local/${t.id}`, settings.overlayPort) ?? ""} alt="" loading="lazy" /> : <span className="q-art art-placeholder"><Icon name="music" size={18} /></span>}
                <span className="q-text">
                  <span className="q-title">{t.title}</span>
                  <span className="q-sub">{t.artist || "Unknown artist"}{t.album ? ` · ${t.album}` : ""}</span>
                </span>
                <span className="q-dur">{formatClock(t.durationMs)}</span>
                <span className="q-actions">
                  <button className="icon-btn" title="Play next" onClick={() => enqueue([t.id], true)}><Icon name="top" size={16} /></button>
                  <button className="icon-btn" title="Add to queue" onClick={() => enqueue([t.id], false)}><Icon name="play" size={16} /></button>
                  <PlaylistPicker compact playlists={playlists} onPick={(pid) => addToPlaylist(pid, [t.id])} />
                </span>
              </li>
            ))}
          </ul>
        )}
        {results.length >= 500 && <p className="muted small">Showing the first 500 matches. Narrow the search to see more.</p>}
      </section>
    </div>
  );
}

export function PlaylistPicker({ playlists, onPick, compact = false }: { playlists: Playlist[]; onPick: (id: number) => void; compact?: boolean }) {
  return (
    <select className={compact ? "picker compact" : "picker"} value="" title="Add to playlist"
      onChange={(e) => { if (e.target.value) onPick(Number(e.target.value)); }}>
      <option value="">{compact ? "+ Playlist" : "Add to playlist…"}</option>
      {playlists.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
    </select>
  );
}
