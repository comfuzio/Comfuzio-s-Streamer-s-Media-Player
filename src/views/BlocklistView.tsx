import { useMemo, useState, type FormEvent } from "react";
import { ExternalLink } from "../components/ExternalLink";
import { api } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { BlockEntry } from "../lib/types";

export function BlocklistView() {
  const { blocklist, notify, fail } = useStore();
  const [filter, setFilter] = useState("");
  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState(false);

  // One row per block action (a Spotify block also covers its matched YouTube upload).
  const groups = useMemo(() => {
    const map = new Map<string, BlockEntry[]>();
    for (const e of blocklist) map.set(e.groupId, [...(map.get(e.groupId) ?? []), e]);
    const q = filter.trim().toLowerCase();
    return [...map.values()].filter((g) =>
      !q || g.some((e) => `${e.title ?? ""} ${e.artist ?? ""} ${e.key}`.toLowerCase().includes(q)));
  }, [blocklist, filter]);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!target.trim()) return;
    setBusy(true);
    try {
      const out = await api.blocklist.add(target);
      notify({ level: "info", message: `Blocked ${out.title ?? "song"}` });
      setTarget("");
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="view">
      <form className="request" onSubmit={add}>
        <input value={target} disabled={busy} onChange={(e) => setTarget(e.target.value)} placeholder="Block a YouTube / Spotify link or artist - song" />
        <button className="primary" type="submit" disabled={busy || !target.trim()}>{busy ? "Blocking…" : "Block"}</button>
      </form>
      <section className="card">
        <header className="section-head">
          <h2>Blocked songs</h2>
          <span className="muted">{groups.length}</span>
          <input className="search" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter…" />
        </header>
        {groups.length === 0 ? (
          <p className="empty">{blocklist.length === 0 ? "Nothing blocked yet. Mods can use the block command in chat." : "No matches."}</p>
        ) : (
          <ul className="block-list">
            {groups.map((g) => {
              const head = g[0]!;
              return (
                <li key={head.groupId}>
                  <span className="q-text">
                    <span className="q-title">{head.title ?? head.key}</span>
                    <span className="q-sub">
                      {head.artist && `${head.artist} · `}blocked by {head.blockedBy} · {new Date(head.createdAtMs).toLocaleDateString()}
                      {head.sourceUrl && <> · <ExternalLink href={head.sourceUrl}>source</ExternalLink></>}
                    </span>
                  </span>
                  <span className="muted small">{g.map((e) => e.key).join(", ")}</span>
                  <button onClick={() => api.blocklist.remove(head.key).catch(fail)}>Unblock</button>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}
