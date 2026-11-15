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
