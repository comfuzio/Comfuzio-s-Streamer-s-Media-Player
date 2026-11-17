import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppError,
  AppEvent,
  BlockEntry,
  BlockOutcome,
  ChatPlatform,
  ChatStatus,
  DeviceLogin,
  ExportOutcome,
  ImportOutcome,
  LibraryStatus,
  LibraryTrack,
  Playlist,
  PlaylistDetail,
  ScanSummary,
  OverlayInfo,
  QuotaUsage,
  OutputDevice,
  PlayerSnapshot,
  QueueItem,
  QueueSnapshot,
  RequestOutcome,
  Settings,
  SpotifyStatus,
} from "./types";

export const api = {
  player: {
    snapshot: () => invoke<PlayerSnapshot>("player_snapshot"),
    play: () => invoke<PlayerSnapshot>("player_play"),
    pause: () => invoke<PlayerSnapshot>("player_pause"),
    stop: () => invoke<PlayerSnapshot>("player_stop"),
    skip: () => invoke<QueueItem | null>("player_skip"),
    seek: (positionMs: number) => invoke<PlayerSnapshot>("player_seek", { positionMs }),
    setVolume: (volume: number) => invoke<number>("player_set_volume", { volume }),
  },
  queue: {
    get: () => invoke<QueueSnapshot>("queue_get"),
    request: (input: string) => invoke<RequestOutcome>("queue_request", { input }),
    remove: (queueId: string) => invoke<QueueItem | null>("queue_remove", { queueId }),
    move: (queueId: string, toIndex: number) => invoke<void>("queue_move", { queueId, toIndex }),
    clear: () => invoke<number>("queue_clear"),
  },
  settings: {
    get: () => invoke<Settings>("settings_get"),
    update: (settings: Settings) => invoke<Settings>("settings_update", { settings }),
    setMaxLength: (value: string) => invoke<number>("settings_set_max_length", { value }),
  },
  blocklist: {
    get: () => invoke<BlockEntry[]>("blocklist_get"),
    add: (target: string | null) => invoke<BlockOutcome>("blocklist_add", { target }),
    remove: (key: string) => invoke<BlockEntry[]>("blocklist_remove", { key }),
  },
  audioDevices: () => invoke<OutputDevice[]>("audio_devices"),
  spotify: {
    status: () => invoke<SpotifyStatus>("spotify_status"),
    login: () => invoke<SpotifyStatus>("spotify_login"),
    logout: () => invoke<SpotifyStatus>("spotify_logout"),
  },
  ytDlpVersion: () => invoke<string>("ytdlp_version"),
  overlay: {
    info: () => invoke<OverlayInfo>("overlay_info"),
  },
  library: {
    status: () => invoke<LibraryStatus>("library_status"),
    scan: () => invoke<ScanSummary>("library_scan"),
    search: (query: string) => invoke<LibraryTrack[]>("library_search", { query }),
    enqueue: (ids: number[], next: boolean) => invoke<number>("library_enqueue", { ids, next }),
  },
  playlists: {
    list: () => invoke<Playlist[]>("playlists_list"),
    get: (id: number) => invoke<PlaylistDetail>("playlist_get", { id }),
    create: (name: string) => invoke<Playlist>("playlist_create", { name }),
    rename: (id: number, name: string) => invoke<Playlist>("playlist_rename", { id, name }),
    remove: (id: number) => invoke<void>("playlist_delete", { id }),
    addLibrary: (id: number, libraryIds: number[]) => invoke<number>("playlist_add_library", { id, libraryIds }),
    addLink: (id: number, input: string) => invoke<void>("playlist_add_link", { id, input }),
    addQueue: (id: number) => invoke<number>("playlist_add_queue", { id }),
    removeItem: (itemId: number) => invoke<void>("playlist_remove_item", { itemId }),
    moveItem: (itemId: number, toIndex: number) => invoke<void>("playlist_move_item", { itemId, toIndex }),
    enqueue: (id: number, shuffle: boolean) => invoke<number>("playlist_enqueue", { id, shuffle }),
    importM3u: (path: string) => invoke<ImportOutcome>("playlist_import_m3u", { path }),
    importUrl: (url: string) => invoke<ImportOutcome>("playlist_import_url", { url }),
    sync: (id: number) => invoke<ImportOutcome>("playlist_sync", { id }),
    exportM3u: (id: number, path: string) => invoke<ExportOutcome>("playlist_export_m3u", { id, path }),
    exportYoutube: (id: number) => invoke<ExportOutcome>("playlist_export_youtube", { id }),
    exportSpotify: (id: number) => invoke<ExportOutcome>("playlist_export_spotify", { id }),
  },
  chat: {
    statuses: () => invoke<ChatStatus[]>("chat_statuses"),
    reconnect: (platform: ChatPlatform) => invoke<void>("chat_reconnect", { platform }),
    twitchLogin: () => invoke<DeviceLogin>("twitch_login"),
    twitchLogout: () => invoke<void>("twitch_logout"),
    youtubeLogin: () => invoke<string>("youtube_login"),
    youtubeLogout: () => invoke<void>("youtube_logout"),
    youtubeQuota: () => invoke<QuotaUsage>("youtube_quota"),
  },
};

export function onAppEvent(handler: (event: AppEvent) => void): Promise<UnlistenFn> {
  return listen<AppEvent>("app-event", (e) => handler(e.payload));
}

export function errorMessage(err: unknown): string {
  if (typeof err === "object" && err !== null && "message" in err) return (err as AppError).message;
  return String(err);
}

export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Local artwork is served by the overlay server; turn its relative path into a URL. */
export function artUrl(url: string | null | undefined, overlayPort: number | undefined): string | null {
  if (!url) return null;
  return url.startsWith("/") ? `http://127.0.0.1:${overlayPort ?? 3847}${url}` : url;
}
