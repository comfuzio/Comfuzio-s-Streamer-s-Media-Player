import { useState, type DragEvent, type FormEvent } from "react";
import { ExternalLink } from "../components/ExternalLink";
import { Icon } from "../components/Icon";
import { api, artUrl, formatClock } from "../lib/ipc";
import { usePlaybackPosition, useStore } from "../lib/store";
import type { PlayerStatus, QueueItem } from "../lib/types";

const STATUS_LABEL: Record<PlayerStatus, string> = {
  stopped: "Stopped",
  idle: "Waiting for requests",
  loading: "Loading…",
  playing: "Playing",
  paused: "Paused",
};

function NowPlaying() {
  const { player, settings, fail } = useStore();
  const position = usePlaybackPosition();
  const [scrub, setScrub] = useState<number | null>(null);
  const item = player?.current;
  const status = player?.status ?? "stopped";
  const running = status === "playing" || status === "loading";
  const volume = settings?.volume ?? player?.volume ?? 0;

  return (
    <section className="card now-playing">
      <div className={`vinyl${status === "playing" ? " spinning" : ""}${item ? "" : " empty"}`} aria-hidden="true">
        <div className="vinyl-disc">
          <div className="vinyl-label">
            {item?.track.artworkUrl ? <img src={artUrl(item.track.artworkUrl, settings?.overlayPort) ?? ""} alt="" referrerPolicy="no-referrer" /> : <Icon name="music" size={26} />}
          </div>
        </div>
        <div className="vinyl-sheen" />
      </div>
      <div className="np-body">
        <span className={`pill pill-${status}`}>{STATUS_LABEL[status]}</span>
        <h2 className="np-title" title={item?.track.title}>{item?.track.title ?? "Nothing playing"}</h2>
        <div className="np-artist">{item?.track.artist ?? "Start the player, then request a song below or from chat."}</div>
        {item && (
          <div className="np-meta">
            {item.autoplay ? <>Auto-DJ from <strong>{item.requester.displayName}</strong></> : <>requested by <strong>{item.requester.displayName}</strong></>}
            {" · "}
            {item.track.origin.kind === "local"
              ? <span title={item.track.sourceUrl}>Local file</span>
              : <ExternalLink href={item.track.sourceUrl}>{item.track.origin.kind === "spotify" ? "Spotify" : "YouTube"}</ExternalLink>}
          </div>
        )}
        <div className="np-progress">
          <span>{formatClock(scrub ?? position)}</span>
          <input
            type="range" min={0} max={Math.max(player?.durationMs ?? 0, 1)} step={1000}
            value={scrub ?? position} disabled={!item || status === "loading"}
            onChange={(e) => setScrub(Number(e.target.value))}
            onPointerUp={() => { if (scrub !== null) api.player.seek(scrub).catch(fail).finally(() => setScrub(null)); }}
            onKeyUp={() => { if (scrub !== null) api.player.seek(scrub).catch(fail).finally(() => setScrub(null)); }}
          />
          <span>{formatClock(player?.durationMs ?? 0)}</span>
        </div>
        <div className="transport">
          {running ? (
            <button className="btn-round primary" title="Pause" onClick={() => api.player.pause().catch(fail)}><Icon name="pause" size={22} /></button>
          ) : (
            <button className="btn-round primary" title={status === "paused" ? "Resume" : "Start"} onClick={() => api.player.play().catch(fail)}><Icon name="play" size={22} /></button>
          )}
          <button className="btn-round" title="Stop" onClick={() => api.player.stop().catch(fail)} disabled={status === "stopped"}><Icon name="stop" /></button>
          <button className="btn-round" title="Skip" onClick={() => api.player.skip().catch(fail)}><Icon name="skip" /></button>
          <button className="btn-round" title="Block and skip" onClick={() => api.blocklist.add(null).catch(fail)} disabled={!item}><Icon name="ban" /></button>
          <label className="volume" title="Volume">
            <Icon name="volume" />
            <input type="range" min={0} max={100} value={volume}
              onChange={(e) => api.player.setVolume(Number(e.target.value)).catch(fail)} />
            <span>{volume}%</span>
          </label>
        </div>
      </div>
    </section>
  );
}

function RequestBox() {
  const { notify, fail } = useStore();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!text.trim() || busy) return;
    setBusy(true);
    try {
      const out = await api.queue.request(text);
      notify({ level: "info", message: `Queued #${out.position}: ${out.item.track.artist} - ${out.item.track.title}` });
      setText("");
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="request" onSubmit={submit}>
      <input value={text} onChange={(e) => setText(e.target.value)} disabled={busy}
        placeholder="Paste a YouTube / Spotify link, or type artist - song" />
      <button className="primary" type="submit" disabled={busy || !text.trim()}>{busy ? "Finding…" : "Add to queue"}</button>
    </form>
  );
}

function QueueRow({ item, index, onDragStart, onDragOver, onDrop, dragging, over, port }: {
  port: number | undefined;
  item: QueueItem; index: number; dragging: boolean; over: boolean;
  onDragStart: (e: DragEvent, index: number) => void;
  onDragOver: (e: DragEvent, index: number) => void;
  onDrop: (e: DragEvent, index: number) => void;
}) {
  const { fail } = useStore();
  return (
    <li
      className={`queue-row${dragging ? " dragging" : ""}${over ? " drop-target" : ""}`}
      draggable
      onDragStart={(e) => onDragStart(e, index)}
      onDragOver={(e) => onDragOver(e, index)}
      onDrop={(e) => onDrop(e, index)}
    >
      <span className="grip" title="Drag to reorder"><Icon name="grip" size={16} /></span>
      <span className="q-index">{index + 1}</span>
      {item.track.artworkUrl ? <img className="q-art" src={artUrl(item.track.artworkUrl, port) ?? ""} alt="" referrerPolicy="no-referrer" /> : <span className="q-art art-placeholder"><Icon name="music" size={18} /></span>}
      <span className="q-text">
        <span className="q-title">{item.track.title}</span>
        <span className="q-sub">{item.track.artist} · {item.requester.displayName}</span>
      </span>
      <span className="q-dur">{formatClock(item.track.durationMs)}</span>
      <span className="q-actions">
        <button className="icon-btn" title="Move to top" disabled={index === 0} onClick={() => api.queue.move(item.queueId, 0).catch(fail)}><Icon name="top" size={16} /></button>
        <button className="icon-btn" title="Block" onClick={() => api.blocklist.add(item.track.sourceUrl).catch(fail)}><Icon name="ban" size={16} /></button>
        <button className="icon-btn danger" title="Remove" onClick={() => api.queue.remove(item.queueId).catch(fail)}><Icon name="trash" size={16} /></button>
      </span>
    </li>
  );
}

function QueueManager() {
  const { queue, settings, fail } = useStore();
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);

  const onDragStart = (e: DragEvent, index: number) => {
    setDragIndex(index);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", String(index));
  };
  const onDragOver = (e: DragEvent, index: number) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (overIndex !== index) setOverIndex(index);
  };
  const onDrop = (e: DragEvent, index: number) => {
    e.preventDefault();
    const from = dragIndex;
    setDragIndex(null);
    setOverIndex(null);
    const item = from !== null ? queue.items[from] : undefined;
    if (item && from !== index) api.queue.move(item.queueId, index).catch(fail);
  };

  return (
    <section className="card queue-card" onDragEnd={() => { setDragIndex(null); setOverIndex(null); }}>
      <header className="section-head">
        <h2>Queue</h2>
        <span className="muted">{queue.items.length} song{queue.items.length === 1 ? "" : "s"} · {formatClock(queue.totalDurationMs)}</span>
        <button className="ghost danger" disabled={queue.items.length === 0}
          onClick={() => { if (confirm(`Remove all ${queue.items.length} songs from the queue?`)) api.queue.clear().catch(fail); }}>
          Clear
        </button>
      </header>
      {queue.items.length === 0 ? (
        <p className="empty">
          The queue is empty. Requests from chat and the box above land here.
          {settings?.autoDj.enabled ? " Auto-DJ fills the gaps." : " Set up Auto-DJ in Playlists to keep music playing between requests."}
        </p>
      ) : (
        <ol className="queue-list">
          {queue.items.map((item, i) => (
            <QueueRow key={item.queueId} item={item} index={i} port={settings?.overlayPort} dragging={dragIndex === i} over={overIndex === i && dragIndex !== i}
              onDragStart={onDragStart} onDragOver={onDragOver} onDrop={onDrop} />
          ))}
        </ol>
      )}
    </section>
  );
}

export function PlayerView() {
  return (
    <div className="view player-view">
      <NowPlaying />
      <RequestBox />
      <QueueManager />
    </div>
  );
}
