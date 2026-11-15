import { useEffect, useRef, useState } from "react";
import { Icon } from "../components/Icon";
import { api } from "../lib/ipc";
import { useStore } from "../lib/store";
import type { OverlayInfo, OverlaySettings, OverlayTheme } from "../lib/types";

const THEMES: { id: OverlayTheme; name: string; blurb: string; palette: Pick<OverlaySettings, "accentColor" | "textColor" | "backgroundColor"> }[] = [
  { id: "minimal", name: "Minimal", blurb: "Text only, no card", palette: { accentColor: "#ffffff", textColor: "#ffffff", backgroundColor: "#00000000" } },
  { id: "neon", name: "Neon", blurb: "Glowing outline", palette: { accentColor: "#00e5ff", textColor: "#ffffff", backgroundColor: "#0a0014d9" } },
  { id: "glass", name: "Glass", blurb: "Frosted blur card", palette: { accentColor: "#9b6bff", textColor: "#ffffff", backgroundColor: "#14121fb3" } },
  { id: "vinyl", name: "Vinyl", blurb: "Spinning record", palette: { accentColor: "#ffb347", textColor: "#fff7ec", backgroundColor: "#1b1410e6" } },
];

const TOGGLES: { key: keyof OverlaySettings; label: string }[] = [
  { key: "showArtwork", label: "Album art" },
  { key: "showRequester", label: "Requester name" },
  { key: "showProgress", label: "Progress bar" },
  { key: "showUpNext", label: "Up next" },
  { key: "hideWhenIdle", label: "Hide when nothing plays" },
  { key: "animate", label: "Animations" },
];

/** Splits `#rrggbbaa` into an `<input type=color>` value and 0-100 opacity. */
function splitAlpha(hex: string): [string, number] {
  const rgb = hex.slice(0, 7);
  const a = hex.length === 9 ? parseInt(hex.slice(7, 9), 16) : 255;
  return [rgb, Math.round((a / 255) * 100)];
}

function joinAlpha(rgb: string, opacity: number): string {
  const a = Math.round((Math.min(100, Math.max(0, opacity)) / 100) * 255);
  return `${rgb}${a.toString(16).padStart(2, "0")}`;
}

/** Renders the overlay at its real OBS size and scales it down to fit the panel. */
function Preview({ src, width, height }: { src: string; width: number; height: number }) {
  const box = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState(1);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      if (entry) setFit(Math.min(1, entry.contentRect.width / width));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [width]);
  return (
    <div ref={box} className="preview-frame" style={{ height: Math.max(220, height * fit + 24) }}>
      <iframe title="Overlay preview" src={src}
        style={{ width, height: Math.max(220 / fit, height + 24), transform: `scale(${fit})`, transformOrigin: "top left" }} />
    </div>
  );
}

export function OverlayView() {
  const { settings, patchSettings, notify, fail } = useStore();
  const [info, setInfo] = useState<OverlayInfo | null>(null);
  const [previewKey, setPreviewKey] = useState(0);

  useEffect(() => {
    api.overlay.info().then(setInfo).catch(fail);
  }, [fail, settings?.overlayPort]);

  if (!settings) return null;
  const o = settings.overlay;
  const set = (patch: Partial<OverlaySettings>, debounce = 0) =>
    patchSettings((s) => ({ ...s, overlay: { ...s.overlay, ...patch } }), debounce);
  const [bgRgb, bgAlpha] = splitAlpha(o.backgroundColor);
  const url = info?.url ?? `http://127.0.0.1:${settings.overlayPort}/overlay`;
  const obsWidth = Math.ceil((o.width + 32) * (o.scale / 100));
  const obsHeight = Math.ceil(200 * (o.scale / 100));

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      notify({ level: "info", message: "Overlay URL copied. Add it as a Browser Source in OBS." });
    } catch (err) {
      fail(err);
    }
  };

  return (
    <div className="view overlay-view">
      <section className="card">
        <header className="section-head">
          <h2>OBS Browser Source</h2>
          <span className={`pill ${info?.running ? "pill-playing" : "pill-stopped"}`}>{info?.running ? "Server running" : "Server offline"}</span>
        </header>
        <div className="url-row">
          <code>{url}</code>
          <button className="primary" onClick={() => copy(url)}><Icon name="copy" size={16} /> Copy URL</button>
        </div>
        <p className="muted small">
          In OBS: Sources → + → Browser, paste the URL, set width {obsWidth} and height {obsHeight}. Changes below apply to OBS
          instantly. Add <code>?theme=neon</code> (or minimal, glass, vinyl) to use a different theme in another scene.
        </p>
      </section>

      <div className="builder">
        <section className="card builder-controls">
          <h2>Theme</h2>
          <div className="theme-grid">
            {THEMES.map((t) => (
              <button key={t.id} className={`theme-card theme-${t.id}${o.theme === t.id ? " selected" : ""}`}
                onClick={() => set({ theme: t.id, ...t.palette })}>
                <span className="theme-swatch" style={{ background: t.palette.backgroundColor === "#00000000" ? "transparent" : t.palette.backgroundColor, borderColor: t.palette.accentColor }}>
                  <span style={{ background: t.palette.accentColor }} />
                </span>
                <strong>{t.name}</strong>
                <span className="muted small">{t.blurb}</span>
              </button>
            ))}
          </div>

          <h2>Colors</h2>
          <div className="color-grid">
            <label>Accent<input type="color" value={o.accentColor.slice(0, 7)} onChange={(e) => set({ accentColor: e.target.value }, 150)} /></label>
            <label>Text<input type="color" value={o.textColor.slice(0, 7)} onChange={(e) => set({ textColor: e.target.value }, 150)} /></label>
            <label>Background<input type="color" value={bgRgb} onChange={(e) => set({ backgroundColor: joinAlpha(e.target.value, bgAlpha) }, 150)} /></label>
            <label className="span-2">Background opacity · {bgAlpha}%
              <input type="range" min={0} max={100} value={bgAlpha} onChange={(e) => set({ backgroundColor: joinAlpha(bgRgb, Number(e.target.value)) }, 150)} />
            </label>
          </div>

          <h2>Typography &amp; size</h2>
          <div className="form-grid">
            <label>Font
              <select value={o.font} onChange={(e) => set({ font: e.target.value })}>
                {(info?.fonts ?? [o.font]).map((f) => <option key={f} value={f}>{f === "system" ? "System default" : f}</option>)}
              </select>
            </label>
            <label>Scale · {o.scale}%<input type="range" min={50} max={200} step={5} value={o.scale} onChange={(e) => set({ scale: Number(e.target.value) }, 150)} /></label>
            <label>Width · {o.width}px<input type="range" min={240} max={1200} step={10} value={o.width} onChange={(e) => set({ width: Number(e.target.value) }, 150)} /></label>
            <label>Corner radius · {o.cornerRadius}px<input type="range" min={0} max={48} value={o.cornerRadius} onChange={(e) => set({ cornerRadius: Number(e.target.value) }, 150)} /></label>
          </div>

          <h2>Show</h2>
          <div className="toggle-grid">
            {TOGGLES.map((t) => (
              <label key={t.key} className="check">
                <input type="checkbox" checked={Boolean(o[t.key])} onChange={(e) => set({ [t.key]: e.target.checked } as Partial<OverlaySettings>)} />
                {t.label}
              </label>
            ))}
          </div>
        </section>

        <section className="card builder-preview">
          <header className="section-head">
            <h2>Live preview</h2>
            <button className="ghost" onClick={() => setPreviewKey((k) => k + 1)}>Reload</button>
          </header>
          {info?.running ? (
            <Preview key={previewKey} src={`${url}?preview=1&demo=1`} width={obsWidth} height={obsHeight} />
          ) : (
            <div className="preview-frame"><p className="empty">The overlay server isn't running. Check the port in Settings.</p></div>
          )}
          <p className="muted small">Shows the real track while one plays, otherwise a sample track.</p>
        </section>
      </div>
    </div>
  );
}
