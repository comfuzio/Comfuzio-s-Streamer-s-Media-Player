import { useEffect, useState } from "react";
import { Icon, type IconName } from "./components/Icon";
import { StoreProvider, useStore } from "./lib/store";
import { checkForUpdate, useUpdateState } from "./lib/updates";
import { AboutView } from "./views/AboutView";
import { BlocklistView } from "./views/BlocklistView";
import { ChatView } from "./views/ChatView";
import { LibraryView } from "./views/LibraryView";
import { OverlayView } from "./views/OverlayView";
import { PlayerView } from "./views/PlayerView";
import { PlaylistsView } from "./views/PlaylistsView";
import { SettingsView } from "./views/SettingsView";

type ViewId = "player" | "library" | "playlists" | "overlay" | "chat" | "blocklist" | "settings" | "about";

const NAV: { id: ViewId; label: string; icon: IconName }[] = [
  { id: "player", label: "Player & queue", icon: "music" },
  { id: "library", label: "Library", icon: "folder" },
  { id: "playlists", label: "Playlists", icon: "list" },
  { id: "overlay", label: "Overlay", icon: "layers" },
  { id: "chat", label: "Chat bot", icon: "chat" },
  { id: "blocklist", label: "Blocklist", icon: "ban" },
  { id: "settings", label: "Settings", icon: "gear" },
  { id: "about", label: "About", icon: "info" },
];

const VIEW_KEY = "comfuzio.view";

function readView(): ViewId {
  try {
    const v = localStorage.getItem(VIEW_KEY);
    if (v && NAV.some((n) => n.id === v)) return v as ViewId;
  } catch {
    // storage unavailable: fall back to the default view
  }
  return "player";
}

function Notices() {
  const { notices, dismiss } = useStore();
  return (
    <aside className="notices" aria-live="polite">
      {notices.map((n) => (
        <button key={n.id} className={`notice notice-${n.level}`} onClick={() => dismiss(n.id)}>{n.message}</button>
      ))}
    </aside>
  );
}

function SidebarNowPlaying() {
  const { player, queue } = useStore();
  const item = player?.current;
  return (
    <div className="side-np">
      <span className={`dot dot-${player?.status ?? "stopped"}`} />
      <span className="side-np-text">
        <span className="side-np-title">{item ? item.track.title : player?.status === "idle" ? "Waiting for requests" : "Stopped"}</span>
        <span className="muted small">{item?.autoplay ? "Auto-DJ · " : ""}{queue.items.length} in queue</span>
      </span>
    </div>
  );
}

function Shell() {
  const { settings, notify } = useStore();
  const [view, setView] = useState<ViewId>(readView);
  const update = useUpdateState();
  const autoCheck = settings?.autoCheckUpdates;

  // One quiet update check shortly after startup.
  useEffect(() => {
    if (!autoCheck) return;
    const t = window.setTimeout(() => {
      checkForUpdate().then((u) => {
        if (u) notify({ level: "info", message: `Version ${u.version} is available. Open About to install it.` });
      });
    }, 8000);
    return () => window.clearTimeout(t);
  }, [autoCheck, notify]);
  useEffect(() => {
    try {
      localStorage.setItem(VIEW_KEY, view);
    } catch {
      // ignore
    }
  }, [view]);

  return (
    <div className="shell">
      <nav className="sidebar">
        <div className="brand">
          <img src="/app-icon.svg" alt="" width={30} height={30} />
          <span>Comfuzio<small>Streamer's Media Player</small></span>
        </div>
        {NAV.map((n) => (
          <button key={n.id} className={`nav-item${view === n.id ? " active" : ""}`} onClick={() => setView(n.id)}>
            <Icon name={n.icon} /> {n.label}
            {n.id === "about" && update.phase === "available" && <span className="nav-badge">Update</span>}
          </button>
        ))}
        <div className="spacer" />
        <SidebarNowPlaying />
      </nav>
      <main className="content">
        {!settings ? <p className="empty">Loading…</p> : (
          <>
            {view === "player" && <PlayerView />}
            {view === "library" && <LibraryView />}
            {view === "playlists" && <PlaylistsView />}
            {view === "overlay" && <OverlayView />}
            {view === "chat" && <ChatView />}
            {view === "blocklist" && <BlocklistView />}
            {view === "settings" && <SettingsView />}
            {view === "about" && <AboutView />}
          </>
        )}
      </main>
      <Notices />
    </div>
  );
}

export function App() {
  return (
    <StoreProvider>
      <Shell />
    </StoreProvider>
  );
}
