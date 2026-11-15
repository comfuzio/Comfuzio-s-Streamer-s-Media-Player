// Global dashboard state, fed by backend events (no polling).

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, errorMessage, onAppEvent } from "./ipc";
import type { BlockEntry, Notice, PlayerSnapshot, QueueSnapshot, Settings } from "./types";

export interface TimedNotice extends Notice {
  id: number;
}

interface Store {
  player: PlayerSnapshot | null;
  /** performance.now() when `player` arrived, for local progress interpolation. */
  playerReceivedAt: number;
  queue: QueueSnapshot;
  settings: Settings | null;
  blocklist: BlockEntry[];
  notices: TimedNotice[];
  /** Applies `update` optimistically and saves (debounced for sliders/pickers). */
  patchSettings: (update: (s: Settings) => Settings, debounceMs?: number) => void;
  notify: (n: Notice) => void;
  dismiss: (id: number) => void;
  fail: (err: unknown) => void;
}

const StoreContext = createContext<Store | null>(null);

export function useStore(): Store {
  const s = useContext(StoreContext);
  if (!s) throw new Error("useStore outside <StoreProvider>");
  return s;
}

let noticeSeq = 0;

export function StoreProvider({ children }: { children: ReactNode }) {
  const [player, setPlayer] = useState<PlayerSnapshot | null>(null);
  const [playerReceivedAt, setPlayerReceivedAt] = useState(() => performance.now());
  const [queue, setQueue] = useState<QueueSnapshot>({ items: [], totalDurationMs: 0 });
  const [settings, setSettings] = useState<Settings | null>(null);
  const [blocklist, setBlocklist] = useState<BlockEntry[]>([]);
  const [notices, setNotices] = useState<TimedNotice[]>([]);

  const latest = useRef<Settings | null>(null);
  const saveTimer = useRef<number | undefined>(undefined);
  const pendingSave = useRef(false);

  const dismiss = useCallback((id: number) => setNotices((prev) => prev.filter((n) => n.id !== id)), []);
  const notify = useCallback(
    (n: Notice) => {
      const id = ++noticeSeq;
      setNotices((prev) => [{ ...n, id }, ...prev].slice(0, 5));
      window.setTimeout(() => dismiss(id), n.level === "error" ? 9000 : 5000);
    },
    [dismiss],
  );
  const fail = useCallback((err: unknown) => notify({ level: "error", message: errorMessage(err) }), [notify]);

  const receiveSettings = useCallback((s: Settings) => {
    // Don't let an echo of an older save clobber edits that haven't been sent yet.
    if (pendingSave.current) return;
    latest.current = s;
    setSettings(s);
  }, []);

  const flush = useCallback(() => {
    saveTimer.current = undefined;
    const next = latest.current;
    if (!next) return;
    api.settings
      .update(next)
      .then((saved) => {
        pendingSave.current = saveTimer.current !== undefined;
        if (!pendingSave.current) {
          latest.current = saved;
          setSettings(saved);
        }
      })
      .catch((err) => {
        pendingSave.current = false;
        fail(err);
        api.settings.get().then(receiveSettings).catch(fail);
      });
  }, [fail, receiveSettings]);

  const patchSettings = useCallback(
    (update: (s: Settings) => Settings, debounceMs = 0) => {
      if (!latest.current) return;
      const next = update(latest.current);
      latest.current = next;
      setSettings(next);
      pendingSave.current = true;
      window.clearTimeout(saveTimer.current);
      if (debounceMs > 0) saveTimer.current = window.setTimeout(flush, debounceMs);
      else flush();
    },
    [flush],
  );

  useEffect(() => {
    const unlisten = onAppEvent((e) => {
      switch (e.type) {
        case "player":
          setPlayer(e.payload);
          setPlayerReceivedAt(performance.now());
          break;
        case "queue": setQueue(e.payload); break;
        case "settings": receiveSettings(e.payload); break;
        case "blocklistChanged": api.blocklist.get().then(setBlocklist).catch(fail); break;
        case "notice": notify(e.payload); break;
        default: break;
      }
    });
    Promise.all([api.player.snapshot(), api.queue.get(), api.settings.get(), api.blocklist.get()])
      .then(([p, q, s, b]) => {
        setPlayer(p);
        setPlayerReceivedAt(performance.now());
        setQueue(q);
        receiveSettings(s);
        setBlocklist(b);
      })
      .catch(fail);
    return () => { unlisten.then((fn) => fn()); };
  }, [fail, notify, receiveSettings]);

  const value = useMemo<Store>(
    () => ({ player, playerReceivedAt, queue, settings, blocklist, notices, patchSettings, notify, dismiss, fail }),
    [player, playerReceivedAt, queue, settings, blocklist, notices, patchSettings, notify, dismiss, fail],
  );
  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

/** Interpolated playback position, re-rendering only the caller each frame while playing. */
export function usePlaybackPosition(): number {
  const { player, playerReceivedAt } = useStore();
  const [, setFrame] = useState(0);
  const playing = player?.status === "playing";
  useEffect(() => {
    if (!playing) return;
    let id = requestAnimationFrame(function loop() {
      setFrame((f) => (f + 1) % 1_000_000);
      id = requestAnimationFrame(loop);
    });
    return () => cancelAnimationFrame(id);
  }, [playing]);
  if (!player) return 0;
  const pos = player.positionMs + (playing ? performance.now() - playerReceivedAt : 0);
  return player.durationMs > 0 ? Math.min(pos, player.durationMs) : pos;
}
