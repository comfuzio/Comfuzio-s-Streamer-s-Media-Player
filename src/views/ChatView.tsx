import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, onAppEvent } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { ChatPlatform, ChatStatus, DeviceLogin, QuotaUsage, Settings } from "../lib/types";

const STATE_LABEL: Record<ChatStatus["state"], string> = {
  disabled: "Off",
  notConfigured: "Needs setup",
  connecting: "Connecting…",
  connected: "Connected",
  waitingForStream: "Waiting for stream",
  error: "Error",
};

function StatusLine({ status }: { status: ChatStatus | undefined }) {
  if (!status) return null;
  const parts = [status.account && `as ${status.account}`, status.channel && `in ${status.channel}`].filter(Boolean);
  return (
    <p className={`chat-status chat-${status.state}`}>
      <strong>{STATE_LABEL[status.state]}</strong> {parts.join(" ")}
      {status.detail && <span className="hint"> · {status.detail}</span>}
    </p>
  );
}

export function ChatView() {
  const { settings } = useStore();
  if (!settings) return null;
  return <ChatPanel settings={settings} />;
}

function ChatPanel({ settings }: { settings: Settings }) {
  const { patchSettings, fail: onError } = useStore();
  const [statuses, setStatuses] = useState<Partial<Record<ChatPlatform, ChatStatus>>>({});
  const [device, setDevice] = useState<DeviceLogin | null>(null);
  const [quota, setQuota] = useState<QuotaUsage | null>(null);
  const [ytBusy, setYtBusy] = useState(false);

  useEffect(() => {
    const unlisten = onAppEvent((e) => {
      if (e.type === "chatStatus") {
        setStatuses((prev) => ({ ...prev, [e.payload.platform]: e.payload }));
        if (e.payload.platform === "youTube") api.chat.youtubeQuota().then(setQuota).catch(() => {});
      }
      if (e.type === "chatAuth" && e.payload.platform === "twitch") setDevice(null);
    });
    api.chat.statuses().then((list) => setStatuses(Object.fromEntries(list.map((s) => [s.platform, s])))).catch(onError);
    api.chat.youtubeQuota().then(setQuota).catch(onError);
    return () => { unlisten.then((fn) => fn()); };
  }, [onError]);

  const save = (next: Settings) => patchSettings(() => next);
  const tw = settings.twitch;
  const yt = settings.youtubeChat;
  const ns = settings.commandNamespace;

  return (
    <div className="view"><section className="card">
      <h2>Chat bot</h2>
      <p className="hint">
        Viewers: !{ns}sr, !{ns}song, !{ns}queue, !{ns}wrongsong · Mods: !{ns}start, !{ns}stop, !{ns}skip, !{ns}vol,
        !{ns}volup, !{ns}voldown, !{ns}clear, !{ns}block, !{ns}unblock, !{ns}maxlength
      </p>

      <div className="settings">
        <h3>Twitch</h3>
        <label className="check">
          <input type="checkbox" checked={tw.enabled} onChange={(e) => save({ ...settings, twitch: { ...tw, enabled: e.target.checked } })} />
          Enable Twitch chat
        </label>
        <label>Client ID (Twitch app, client type "Public")
          <input defaultValue={tw.clientId ?? ""} key={`tw-id-${tw.clientId}`}
            onBlur={(e) => save({ ...settings, twitch: { ...tw, clientId: e.target.value || null } })} />
        </label>
        <label>Channel (blank = the signed-in account's own channel)
          <input defaultValue={tw.channel ?? ""} key={`tw-ch-${tw.channel}`} placeholder="yourchannel"
            onBlur={(e) => save({ ...settings, twitch: { ...tw, channel: e.target.value || null } })} />
        </label>
        <label className="check">
          <input type="checkbox" checked={tw.replyInChat} onChange={(e) => save({ ...settings, twitch: { ...tw, replyInChat: e.target.checked } })} />
          Reply in chat
        </label>
        <StatusLine status={statuses.twitch} />
        {device && (
          <p className="device-code">
            Enter code <strong>{device.userCode}</strong> at <button className="small" onClick={() => openUrl(device.verificationUri).catch(onError)}>open activation page</button>
          </p>
        )}
        <span className="row">
          <button disabled={!tw.clientId} onClick={() => api.chat.twitchLogin().then(setDevice).catch(onError)}>Sign in with Twitch</button>
          <button onClick={() => api.chat.twitchLogout().catch(onError)}>Sign out</button>
          <button onClick={() => api.chat.reconnect("twitch").catch(onError)}>Reconnect</button>
        </span>

        <h3>YouTube</h3>
        <label className="check">
          <input type="checkbox" checked={yt.enabled} onChange={(e) => save({ ...settings, youtubeChat: { ...yt, enabled: e.target.checked } })} />
          Enable YouTube live chat
        </label>
        <label>OAuth client ID (Google Cloud, "Desktop app")
          <input defaultValue={yt.clientId ?? ""} key={`yt-id-${yt.clientId}`}
            onBlur={(e) => save({ ...settings, youtubeChat: { ...yt, clientId: e.target.value || null } })} />
        </label>
        <label>OAuth client secret
          <input type="password" defaultValue={yt.clientSecret ?? ""} key={`yt-secret-${yt.clientSecret ? "set" : "unset"}`}
            onBlur={(e) => save({ ...settings, youtubeChat: { ...yt, clientSecret: e.target.value || null } })} />
        </label>
        <label className="check">
          <input type="checkbox" checked={yt.replyInChat} onChange={(e) => save({ ...settings, youtubeChat: { ...yt, replyInChat: e.target.checked } })} />
          Reply in chat (20 quota units per reply)
        </label>
        <StatusLine status={statuses.youTube} />
        {quota && <p className="hint">API quota today: {quota.used} / {yt.dailyQuota} units</p>}
        <span className="row">
          <button disabled={!yt.clientId || !yt.clientSecret || ytBusy}
            onClick={() => { setYtBusy(true); api.chat.youtubeLogin().catch(onError).finally(() => setYtBusy(false)); }}>
            {ytBusy ? "Waiting for browser…" : "Sign in with Google"}
          </button>
          <button onClick={() => api.chat.youtubeLogout().catch(onError)}>Sign out</button>
          <button onClick={() => api.chat.reconnect("youTube").catch(onError)}>Reconnect</button>
        </span>

        <label className="check">
          <input type="checkbox" checked={settings.autoStopOnOffline}
            onChange={(e) => save({ ...settings, autoStopOnOffline: e.target.checked })} />
          Stop playback when the stream goes offline
        </label>
      </div>
    </section></div>
  );
}
