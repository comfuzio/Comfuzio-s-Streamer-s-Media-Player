// Mirrors of the Rust IPC types (serde camelCase).

export type TrackOrigin =
  | { kind: "youTube"; videoId: string }
  | { kind: "spotify"; trackId: string; matchedVideoId: string | null }
  | { kind: "local"; path: string; libraryId: number | null };

export interface Track {
  key: string;
  origin: TrackOrigin;
  title: string;
  artist: string;
  album: string | null;
  artworkUrl: string | null;
  durationMs: number;
  sourceUrl: string;
}

export type Platform = "twitch" | "youTube" | "local";

export interface Requester {
  platform: Platform;
  userId: string;
  displayName: string;
}

export interface QueueItem {
  queueId: string;
  track: Track;
  requester: Requester;
  addedAtMs: number;
  /** Played by Auto-DJ from a playlist rather than requested. */
  autoplay: boolean;
}

export type PlayerStatus = "stopped" | "idle" | "loading" | "playing" | "paused";

export interface PlayerSnapshot {
  status: PlayerStatus;
  current: QueueItem | null;
  positionMs: number;
  durationMs: number;
  volume: number;
  updatedAtMs: number;
}

export interface QueueSnapshot {
  items: QueueItem[];
  totalDurationMs: number;
}

export interface RequestOutcome {
  item: QueueItem;
  position: number;
  etaMs: number;
}

export interface BlockEntry {
  key: string;
  groupId: string;
  title: string | null;
  artist: string | null;
  sourceUrl: string | null;
  blockedBy: string;
  createdAtMs: number;
}

export interface BlockOutcome {
  entries: BlockEntry[];
  title: string | null;
  removedFromQueue: number;
  skippedCurrent: boolean;
}

export interface OutputDevice {
  name: string;
  isDefault: boolean;
}

export type SearchProvider = "youTube" | "spotify";

export interface Settings {
  commandNamespace: string;
  volume: number;
  maxLengthSecs: number;
  autoStopOnOffline: boolean;
  outputDevice: string | null;
  overlayPort: number;
  searchProvider: SearchProvider;
  cacheMaxMib: number;
  youtube: {
    ytDlpPath: string | null;
    cookiesFile: string | null;
    cookiesFromBrowser: string | null;
    jsRuntimes: string | null;
  };
  spotify: {
    clientId: string | null;
    redirectPort: number;
    market: string | null;
  };
  queue: {
    maxPerUser: number;
    maxSize: number;
    allowDuplicates: boolean;
  };
  chat: {
    requestCooldownSecs: number;
    infoCooldownSecs: number;
  };
  twitch: {
    enabled: boolean;
    clientId: string | null;
    channel: string | null;
    replyInChat: boolean;
  };
  youtubeChat: {
    enabled: boolean;
    clientId: string | null;
    clientSecret: string | null;
    minPollSecs: number;
    offlineCheckSecs: number;
    dailyQuota: number;
    replyInChat: boolean;
  };
  overlay: OverlaySettings;
  closeToTray: boolean;
  startInTray: boolean;
  library: { folders: string[]; matchRequests: boolean };
  autoDj: { enabled: boolean; playlistId: number | null; shuffle: boolean };
}

export interface LibraryTrack {
  id: number;
  path: string;
  title: string;
  artist: string;
  album: string | null;
  durationMs: number;
  hasArt: boolean;
}

export interface LibraryStatus {
  total: number;
  scanning: boolean;
}

export interface ScanSummary {
  total: number;
  added: number;
  updated: number;
  removed: number;
  failed: number;
  missingFolders: string[];
}

export type SourceKind = "youTube" | "spotify" | "m3u";

export interface Playlist {
  id: number;
  name: string;
  createdAtMs: number;
  updatedAtMs: number;
  source: { kind: SourceKind; reference: string; syncedAtMs: number | null } | null;
  itemCount: number;
  totalDurationMs: number;
}

export interface PlaylistItem {
  id: number;
  position: number;
  track: Track;
  addedAtMs: number;
}

export interface PlaylistDetail {
  playlist: Playlist;
  items: PlaylistItem[];
}

export interface ImportOutcome {
  playlist: Playlist;
  imported: number;
  skipped: string[];
}

export interface ExportOutcome {
  url: string | null;
  exported: number;
  skipped: number;
}

export type OverlayTheme = "minimal" | "neon" | "glass" | "vinyl";

export interface OverlaySettings {
  theme: OverlayTheme;
  accentColor: string;
  textColor: string;
  backgroundColor: string;
  font: string;
  scale: number;
  width: number;
  cornerRadius: number;
  showArtwork: boolean;
  showRequester: boolean;
  showProgress: boolean;
  showUpNext: boolean;
  hideWhenIdle: boolean;
  animate: boolean;
}

export interface OverlayInfo {
  url: string;
  running: boolean;
  fonts: string[];
}

export type ChatPlatform = "twitch" | "youTube";

export type ConnectionState = "disabled" | "notConfigured" | "connecting" | "connected" | "waitingForStream" | "error";

export interface ChatStatus {
  platform: ChatPlatform;
  state: ConnectionState;
  channel: string | null;
  account: string | null;
  detail: string | null;
}

export interface DeviceLogin {
  userCode: string;
  verificationUri: string;
  expiresIn: number;
}

export interface QuotaUsage {
  day: number;
  used: number;
}

export interface SpotifyStatus {
  configured: boolean;
  connected: boolean;
  redirectUri: string;
}

export interface Notice {
  level: "info" | "warning" | "error";
  message: string;
}

export type AppEvent =
  | { type: "player"; payload: PlayerSnapshot }
  | { type: "queue"; payload: QueueSnapshot }
  | { type: "settings"; payload: Settings }
  | { type: "blocklistChanged" }
  | { type: "libraryChanged" }
  | { type: "playlistsChanged" }
  | { type: "spotifyAuth"; payload: { connected: boolean } }
  | { type: "chatStatus"; payload: ChatStatus }
  | { type: "chatAuth"; payload: { platform: ChatPlatform; connected: boolean } }
  | { type: "notice"; payload: Notice };

export interface AppError {
  kind: string;
  message: string;
}
