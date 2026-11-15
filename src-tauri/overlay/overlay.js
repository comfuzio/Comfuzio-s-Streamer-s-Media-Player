// OBS overlay client. Receives pushed state over WebSocket (no polling) and
// interpolates progress locally between updates.
"use strict";

(() => {
  const params = new URLSearchParams(location.search);
  const THEMES = ["minimal", "neon", "glass", "vinyl"];
  const themeOverride = THEMES.includes(params.get("theme")) ? params.get("theme") : null;
  const demo = params.has("demo");
  const SINGLE_WEIGHT_FONTS = ["Bebas Neue", "Press Start 2P"];

  const root = document.getElementById("overlay");
  const $ = (sel) => root.querySelector(sel);
  const el = {
    art: $(".art"), title: $(".title"), titleWrap: $(".title-wrap"), artist: $(".artist"),
    requester: $(".requester"), fill: $(".fill"), elapsed: $(".elapsed"), total: $(".total"),
    statusText: $(".status-text"), upNext: $(".up-next"),
  };
  if (params.has("preview")) document.body.classList.add("preview");

  const DEMO_ITEM = {
    queueId: "demo",
    track: { title: "Midnight City Lights", artist: "The Example Band", durationMs: 214000, artworkUrl: null },
    requester: { displayName: "ViewerName" },
  };

  let player = null;
  let config = null;
  let upNext = null;
  let receivedAt = performance.now();
  let lastQueueId = null;
  let demoStart = performance.now();

  const clock = (ms) => {
    const t = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = String(t % 60).padStart(2, "0");
    return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
  };

  const placeholderArt = () => {
    const accent = (config && config.accentColor) || "#9b6bff";
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><defs><linearGradient id="g" x2="1" y2="1"><stop offset="0" stop-color="${accent}"/><stop offset="1" stop-color="#15102a"/></linearGradient></defs><rect width="100" height="100" fill="url(#g)"/><circle cx="50" cy="50" r="22" fill="none" stroke="#fff8" stroke-width="4"/><circle cx="50" cy="50" r="5" fill="#fff8"/></svg>`;
    return `data:image/svg+xml,${encodeURIComponent(svg)}`;
  };

  function loadFont(font) {
    let link = document.getElementById("gf");
    if (font === "system") {
      if (link) link.remove();
      root.style.setProperty("--font", "system-ui, -apple-system, 'Segoe UI', sans-serif");
      return;
    }
    const family = encodeURIComponent(font).replace(/%20/g, "+");
    const weights = SINGLE_WEIGHT_FONTS.includes(font) ? "" : ":wght@400;600;700;800";
    const href = `https://fonts.googleapis.com/css2?family=${family}${weights}&display=swap`;
    if (!link) {
      link = document.createElement("link");
      link.id = "gf";
      link.rel = "stylesheet";
      document.head.appendChild(link);
    }
    if (link.href !== href) link.href = href;
    root.style.setProperty("--font", `'${font}', system-ui, sans-serif`);
  }

  function applyConfig(c) {
    config = c;
    root.dataset.theme = themeOverride || c.theme;
    const s = root.style;
    s.setProperty("--accent", c.accentColor);
    s.setProperty("--text", c.textColor);
    s.setProperty("--bg", c.backgroundColor);
    s.setProperty("--scale", String(c.scale / 100));
    s.setProperty("--width", `${c.width}px`);
    s.setProperty("--radius", `${c.cornerRadius}px`);
    root.classList.toggle("no-art", !c.showArtwork);
    root.classList.toggle("no-requester", !c.showRequester);
    root.classList.toggle("no-progress", !c.showProgress);
    root.classList.toggle("no-up-next", !c.showUpNext);
    root.classList.toggle("animate", c.animate);
    loadFont(c.font);
    render();
  }

  function currentItem() {
    if (player && player.current) return player.current;
    return demo ? DEMO_ITEM : null;
  }

  function effectiveStatus() {
    if (player && player.current) return player.status;
    return demo ? "playing" : player ? player.status : "stopped";
  }

  function updateMarquee() {
    const wrap = el.titleWrap;
    wrap.classList.remove("overflowing");
    const distance = el.title.scrollWidth - wrap.clientWidth;
    if (distance > 4) {
      wrap.style.setProperty("--marquee-distance", `${distance + 48}px`);
      wrap.style.setProperty("--marquee-duration", `${Math.max(8, distance / 30 + 6)}s`);
      wrap.classList.add("overflowing");
    }
  }

  function render() {
    if (!config) return;
    const item = currentItem();
    const status = effectiveStatus();
    const visible = !!item && (status === "playing" || status === "paused" || status === "loading" || !config.hideWhenIdle);

    root.classList.toggle("hidden", !visible);
    root.classList.toggle("playing", status === "playing");
    root.classList.toggle("paused", status === "paused");
    if (!item) return;

    if (item.queueId !== lastQueueId) {
      lastQueueId = item.queueId;
      demoStart = performance.now();
      el.title.textContent = item.track.title;
      el.artist.textContent = item.track.artist;
      el.art.src = item.track.artworkUrl || placeholderArt();
      el.art.onerror = () => { el.art.onerror = null; el.art.src = placeholderArt(); };
      if (config.animate) {
        root.classList.remove("swap");
        void root.offsetWidth; // restart the animation
        root.classList.add("swap");
      }
      requestAnimationFrame(updateMarquee);
    }
    el.requester.textContent = item.requester ? `requested by ${item.requester.displayName}` : "";
    el.statusText.textContent = status === "paused" ? "Paused" : status === "loading" ? "Loading" : "Now playing";
    el.total.textContent = clock(item.track.durationMs);
    el.upNext.textContent = upNext ? `Up next: ${upNext.artist ? `${upNext.artist} - ` : ""}${upNext.title}` : "";
  }

  function tick() {
    const item = currentItem();
    if (item) {
      const duration = item.track.durationMs || 0;
      let pos;
      if (player && player.current) {
        pos = player.positionMs + (player.status === "playing" ? performance.now() - receivedAt : 0);
      } else {
        pos = (performance.now() - demoStart) % Math.max(duration, 1);
      }
      if (duration > 0) pos = Math.min(pos, duration);
      el.fill.style.width = duration > 0 ? `${(pos / duration) * 100}%` : "0%";
      el.elapsed.textContent = clock(pos);
    }
    requestAnimationFrame(tick);
  }

  function handle(msg) {
    switch (msg.type) {
      case "hello":
        player = msg.payload.player;
        receivedAt = performance.now();
        upNext = msg.payload.upNext;
        applyConfig(msg.payload.config);
        break;
      case "player":
        player = msg.payload;
        receivedAt = performance.now();
        render();
        break;
      case "config":
        applyConfig(msg.payload);
        break;
      case "upNext":
        upNext = msg.payload;
        render();
        break;
    }
  }

  let backoff = 500;
  function connect() {
    const ws = new WebSocket(`ws://${location.host}/overlay/ws`);
    ws.onopen = () => { backoff = 500; };
    ws.onmessage = (e) => {
      try { handle(JSON.parse(e.data)); } catch (err) { console.error("bad overlay message", err); }
    };
    ws.onclose = () => {
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 10000);
    };
  }

  window.addEventListener("resize", () => requestAnimationFrame(updateMarquee));
  connect();
  requestAnimationFrame(tick);
})();
