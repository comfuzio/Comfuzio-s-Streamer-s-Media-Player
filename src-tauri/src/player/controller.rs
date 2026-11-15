//! Playback controller: the single owner of queue + transport state.
//!
//! Every transition bumps `generation`; async work (downloads) and engine
//! events carry the generation they were started for, so anything that
//! finishes after a skip/stop is recognized as stale and dropped.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch, Mutex};

use crate::audio::cache::AudioCache;
use crate::audio::{AudioEngine, EngineEvent};
use crate::error::{AppError, AppResult, Rejection};
use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::model::{BlockEntry, PlayerSnapshot, PlayerStatus, QueueItem, QueueSnapshot, Requester};
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::resolver::Resolver;
use crate::util::duration::{clamp_max_length, parse_human_duration};
use crate::util::now_ms;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOutcome {
    pub item: QueueItem,
    /// 1-based queue position.
    pub position: usize,
    /// Estimated time until it starts playing.
    pub eta_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub count: usize,
    /// Time until the last queued song would start (remaining current + queued).
    pub wait_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockOutcome {
    pub entries: Vec<BlockEntry>,
    pub title: Option<String>,
    pub removed_from_queue: usize,
    pub skipped_current: bool,
}

struct Inner {
    status: PlayerStatus,
    current: Option<QueueItem>,
    queue: VecDeque<QueueItem>,
    generation: u64,
}

pub struct PlaybackController {
    inner: Mutex<Inner>,
    engine: AudioEngine,
    cache: Arc<AudioCache>,
    resolver: Arc<Resolver>,
    db: Database,
    settings: Arc<SettingsStore>,
    bus: EventBus,
    persist_tx: watch::Sender<Vec<QueueItem>>,
}

impl PlaybackController {
    /// Restores the persisted queue and starts in the `Stopped` state.
    pub async fn start(
        db: Database,
        settings: Arc<SettingsStore>,
        resolver: Arc<Resolver>,
        cache: Arc<AudioCache>,
        bus: EventBus,
    ) -> AppResult<Arc<Self>> {
        let restored = db.load_queue().await?;
        let initial = settings.get();
        let (engine_tx, engine_rx) = mpsc::unbounded_channel();
        let engine = AudioEngine::spawn(engine_tx, initial.volume, initial.output_device.clone())?;
        let (persist_tx, persist_rx) = watch::channel(restored.clone());

        let ctrl = Arc::new(Self {
            inner: Mutex::new(Inner {
                status: PlayerStatus::Stopped,
                current: None,
                queue: restored.into(),
                generation: 0,
            }),
            engine,
            cache,
            resolver,
            db: db.clone(),
            settings,
            bus: bus.clone(),
            persist_tx,
        });

        tauri::async_runtime::spawn(persist_loop(db, persist_rx));
        tauri::async_runtime::spawn(engine_event_loop(Arc::downgrade(&ctrl), engine_rx));
        tauri::async_runtime::spawn(settings_loop(Arc::downgrade(&ctrl), bus.subscribe(), initial.output_device));

        {
            let inner = ctrl.inner.lock().await;
            ctrl.publish_player(&inner);
            ctrl.publish_queue(&inner);
        }
        Ok(ctrl)
    }

    // ---------------------------------------------------------------- snapshots

    fn snapshot_of(&self, inner: &Inner) -> PlayerSnapshot {
        let position_ms = match inner.status {
            PlayerStatus::Playing | PlayerStatus::Paused => self.engine.position_ms(),
            _ => 0,
        };
        let duration_ms = inner.current.as_ref().map(|c| c.track.duration_ms).unwrap_or(0);
        PlayerSnapshot {
            status: inner.status,
            current: inner.current.clone(),
            position_ms: if duration_ms > 0 { position_ms.min(duration_ms) } else { position_ms },
            duration_ms,
            volume: self.settings.get().volume,
            updated_at_ms: now_ms(),
        }
    }

    fn queue_snapshot_of(inner: &Inner) -> QueueSnapshot {
        QueueSnapshot {
            items: inner.queue.iter().cloned().collect(),
            total_duration_ms: inner.queue.iter().map(|i| i.track.duration_ms).sum(),
        }
    }

    fn remaining_current_ms(&self, inner: &Inner) -> u64 {
        let snap = self.snapshot_of(inner);
        snap.duration_ms.saturating_sub(snap.position_ms)
    }

    fn publish_player(&self, inner: &Inner) {
        self.bus.emit(AppEvent::Player(self.snapshot_of(inner)));
    }

    fn publish_queue(&self, inner: &Inner) {
        let persisted: Vec<QueueItem> = inner.current.iter().chain(inner.queue.iter()).cloned().collect();
        self.persist_tx.send_replace(persisted);
        self.bus.emit(AppEvent::Queue(Self::queue_snapshot_of(inner)));
    }

    pub async fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot_of(&*self.inner.lock().await)
    }

    pub async fn queue(&self) -> QueueSnapshot {
        Self::queue_snapshot_of(&*self.inner.lock().await)
    }

    pub async fn queue_summary(&self) -> QueueSummary {
        let inner = self.inner.lock().await;
        let queued: u64 = inner.queue.iter().map(|i| i.track.duration_ms).sum();
        QueueSummary { count: inner.queue.len(), wait_ms: self.remaining_current_ms(&inner) + queued }
    }

    // ---------------------------------------------------------------- requests

    /// Resolves and queues a request. `privileged` (broadcaster/mods/local)
    /// bypasses the max length and per-user limits, never the blocklist.
    pub async fn request(self: &Arc<Self>, input: &str, requester: Requester, privileged: bool) -> AppResult<RequestOutcome> {
        let track = self.resolver.resolve(input).await?;
        let settings = self.settings.get();

        if self.db.blocklist_find(track.block_keys()).await?.is_some() {
            return Err(AppError::rejected(Rejection::Blocked { title: track.display_name() }));
        }
        let max_ms = settings.max_length().as_millis() as u64;
        if !privileged && track.duration_ms > max_ms {
            return Err(AppError::rejected(Rejection::TooLong {
                title: track.display_name(),
                duration_ms: track.duration_ms,
                max_ms,
            }));
        }

        let mut inner = self.inner.lock().await;
        if inner.queue.len() >= settings.queue.max_size as usize {
            return Err(AppError::rejected(Rejection::QueueFull { limit: settings.queue.max_size }));
        }
        if !settings.queue.allow_duplicates {
            let dup = inner.current.iter().chain(inner.queue.iter()).any(|i| i.track.key == track.key);
            if dup {
                return Err(AppError::rejected(Rejection::Duplicate { title: track.display_name() }));
            }
        }
        if !privileged {
            let mine = inner.queue.iter().filter(|i| i.requester.is_same_user(&requester)).count();
            if mine >= settings.queue.max_per_user as usize {
                return Err(AppError::rejected(Rejection::UserLimit { limit: settings.queue.max_per_user }));
            }
        }

        let eta_ms = self.remaining_current_ms(&inner) + inner.queue.iter().map(|i| i.track.duration_ms).sum::<u64>();
        let item = QueueItem { queue_id: uuid::Uuid::new_v4().to_string(), track, requester, added_at_ms: now_ms() };
        inner.queue.push_back(item.clone());
        let position = inner.queue.len();
        self.publish_queue(&inner);

        let status = inner.status;
        drop(inner);
        match status {
            PlayerStatus::Idle => self.spawn_advance(),
            PlayerStatus::Playing | PlayerStatus::Paused | PlayerStatus::Loading if position == 1 => self.spawn_prefetch(),
            _ => {}
        }
        Ok(RequestOutcome { item, position, eta_ms })
    }

    /// Removes `requester`'s most recently queued song.
    pub async fn wrong_song(&self, requester: &Requester) -> Option<QueueItem> {
        let mut inner = self.inner.lock().await;
        let idx = inner.queue.iter().rposition(|i| i.requester.is_same_user(requester))?;
        let removed = inner.queue.remove(idx);
        self.publish_queue(&inner);
        removed
    }

    pub async fn remove(&self, queue_id: &str) -> Option<QueueItem> {
        let mut inner = self.inner.lock().await;
        let idx = inner.queue.iter().position(|i| i.queue_id == queue_id)?;
        let removed = inner.queue.remove(idx);
        self.publish_queue(&inner);
        removed
    }

    pub async fn move_item(&self, queue_id: &str, to_index: usize) -> AppResult<()> {
        let mut inner = self.inner.lock().await;
        let from = inner
            .queue
            .iter()
            .position(|i| i.queue_id == queue_id)
            .ok_or_else(|| AppError::NotFound("queue item not found".into()))?;
        let item = inner.queue.remove(from).expect("index from position()");
        let to = to_index.min(inner.queue.len());
        inner.queue.insert(to, item);
        self.publish_queue(&inner);
        Ok(())
    }

    /// Empties the queue (the current track keeps playing). Returns how many were removed.
    pub async fn clear(&self) -> usize {
        let mut inner = self.inner.lock().await;
        let n = inner.queue.len();
        inner.queue.clear();
        self.publish_queue(&inner);
        n
    }

    // ---------------------------------------------------------------- transport

    /// Starts from `Stopped`, resumes from `Paused`; otherwise a no-op.
    pub async fn play(self: &Arc<Self>) -> PlayerSnapshot {
        let mut inner = self.inner.lock().await;
        let start_queue = match inner.status {
            PlayerStatus::Stopped => {
                inner.status = PlayerStatus::Idle;
                self.publish_player(&inner);
                !inner.queue.is_empty()
            }
            PlayerStatus::Paused => {
                self.engine.resume();
                inner.status = PlayerStatus::Playing;
                self.publish_player(&inner);
                false
            }
            PlayerStatus::Idle | PlayerStatus::Loading | PlayerStatus::Playing => false,
        };
        let snapshot = self.snapshot_of(&inner);
        drop(inner);
        if start_queue {
            self.spawn_advance();
        }
        snapshot
    }

    pub async fn pause(&self) -> PlayerSnapshot {
        let mut inner = self.inner.lock().await;
        if inner.status == PlayerStatus::Playing {
            self.engine.pause();
            inner.status = PlayerStatus::Paused;
            self.publish_player(&inner);
        }
        self.snapshot_of(&inner)
    }

    /// Halts playback. The interrupted track goes back to the front of the
    /// queue so the next start replays it.
    pub async fn stop(&self) -> PlayerSnapshot {
        let mut inner = self.inner.lock().await;
        if inner.status == PlayerStatus::Stopped {
            return self.snapshot_of(&inner);
        }
        inner.generation += 1;
        self.engine.stop();
        if let Some(cur) = inner.current.take() {
            inner.queue.push_front(cur);
        }
        inner.status = PlayerStatus::Stopped;
        self.publish_player(&inner);
        self.publish_queue(&inner);
        self.snapshot_of(&inner)
    }

    /// Skips the current track. While stopped, drops the head of the queue instead.
    pub async fn skip(self: &Arc<Self>) -> Option<QueueItem> {
        let mut inner = self.inner.lock().await;
        let skipped = match inner.status {
            PlayerStatus::Playing | PlayerStatus::Paused | PlayerStatus::Loading => {
                inner.generation += 1;
                self.engine.stop();
                let cur = inner.current.take();
                inner.status = PlayerStatus::Loading;
                self.publish_player(&inner);
                drop(inner);
                self.spawn_advance();
                cur
            }
            PlayerStatus::Stopped => {
                let head = inner.queue.pop_front();
                self.publish_queue(&inner);
                head
            }
            PlayerStatus::Idle => None,
        };
        if let Some(item) = &skipped {
            self.record_history(item.clone(), true);
        }
        skipped
    }

    pub async fn seek(&self, position_ms: u64) -> AppResult<PlayerSnapshot> {
        let inner = self.inner.lock().await;
        if !matches!(inner.status, PlayerStatus::Playing | PlayerStatus::Paused) {
            return Err(AppError::Invalid("nothing is playing".into()));
        }
        let duration = inner.current.as_ref().map(|c| c.track.duration_ms).unwrap_or(0);
        let target = if duration > 0 { position_ms.min(duration.saturating_sub(500)) } else { position_ms };
        self.engine.seek(target);
        self.publish_player(&inner);
        Ok(self.snapshot_of(&inner))
    }

    pub async fn set_volume(&self, volume: u8) -> AppResult<u8> {
        let v = volume.min(100);
        self.engine.set_volume(v);
        self.settings.update(|s| s.volume = v).await?;
        Ok(v)
    }

    pub async fn adjust_volume(&self, delta: i16) -> AppResult<u8> {
        let current = i16::from(self.settings.get().volume);
        self.set_volume((current + delta).clamp(0, 100) as u8).await
    }

    /// Parses (`30s`, `5m`, `1h`...) and stores the max request length, clamped to 30 s..1 h.
    pub async fn set_max_length(&self, input: &str) -> AppResult<Duration> {
        let d = clamp_max_length(parse_human_duration(input)?);
        self.settings.update(|s| s.max_length_secs = d.as_secs() as u32).await?;
        Ok(d)
    }

    // ---------------------------------------------------------------- blocklist

    /// Blocks `target` (link or search), or the current track when `None`,
    /// purging matches from the queue and skipping the current track if it matches.
    pub async fn block(self: &Arc<Self>, target: Option<&str>, blocked_by: &str) -> AppResult<BlockOutcome> {
        let (keys, track) = match target.map(str::trim).filter(|t| !t.is_empty()) {
            None => {
                let current = self.inner.lock().await.current.clone();
                let cur = current.ok_or_else(|| AppError::Invalid("nothing is playing".into()))?;
                (cur.track.block_keys(), Some(cur.track))
            }
            Some(t) => self.resolver.identify(t).await?,
        };
        let group_id = uuid::Uuid::new_v4().to_string();
        let created = now_ms();
        let entries: Vec<BlockEntry> = keys
            .iter()
            .map(|key| BlockEntry {
                key: key.clone(),
                group_id: group_id.clone(),
                title: track.as_ref().map(|t| t.title.clone()),
                artist: track.as_ref().map(|t| t.artist.clone()),
                source_url: track.as_ref().map(|t| t.source_url.clone()),
                blocked_by: blocked_by.to_owned(),
                created_at_ms: created,
            })
            .collect();
        self.db.blocklist_insert(entries.clone()).await?;
        self.bus.emit(AppEvent::BlocklistChanged);

        let matches = |item: &QueueItem| item.track.block_keys().iter().any(|k| keys.contains(k));
        let (removed_from_queue, current_blocked) = {
            let mut inner = self.inner.lock().await;
            let before = inner.queue.len();
            inner.queue.retain(|i| !matches(i));
            let removed = before - inner.queue.len();
            if removed > 0 {
                self.publish_queue(&inner);
            }
            let playing = matches!(inner.status, PlayerStatus::Playing | PlayerStatus::Paused | PlayerStatus::Loading);
            (removed, playing && inner.current.as_ref().is_some_and(matches))
        };
        if current_blocked {
            self.skip().await;
        }
        Ok(BlockOutcome {
            entries,
            title: track.map(|t| t.display_name()),
            removed_from_queue,
            skipped_current: current_blocked,
        })
    }

    pub async fn unblock(&self, target: &str) -> AppResult<Vec<BlockEntry>> {
        let (keys, _) = self.resolver.identify(target).await?;
        let removed = self.db.blocklist_remove(keys).await?;
        if removed.is_empty() {
            return Err(AppError::NotFound("that song isn't on the blocklist".into()));
        }
        self.bus.emit(AppEvent::BlocklistChanged);
        Ok(removed)
    }

    pub async fn unblock_key(&self, key: &str) -> AppResult<Vec<BlockEntry>> {
        let removed = self.db.blocklist_remove(vec![key.to_owned()]).await?;
        if !removed.is_empty() {
            self.bus.emit(AppEvent::BlocklistChanged);
        }
        Ok(removed)
    }

    // ---------------------------------------------------------------- internals

    fn spawn_advance(self: &Arc<Self>) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move { this.advance().await });
    }

    fn spawn_prefetch(self: &Arc<Self>) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let (next, protect) = {
                let inner = this.inner.lock().await;
                (inner.queue.front().map(|i| i.track.playback_video_id().to_owned()), Self::protected_ids(&inner))
            };
            if let Some(id) = next {
                if let Err(err) = this.cache.fetch(&id, &protect).await {
                    tracing::warn!(%err, video_id = %id, "prefetch failed");
                }
            }
        });
    }

    fn protected_ids(inner: &Inner) -> Vec<String> {
        inner.current.iter().chain(inner.queue.iter().take(2)).map(|i| i.track.playback_video_id().to_owned()).collect()
    }

    fn record_history(&self, item: QueueItem, skipped: bool) {
        let db = self.db.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = db.push_history(item, skipped).await {
                tracing::warn!(%err, "failed to record history");
            }
        });
    }

    /// Pops and plays the next queue item, skipping items that fail to load.
    async fn advance(self: &Arc<Self>) {
        loop {
            let (item, generation, protect) = {
                let mut inner = self.inner.lock().await;
                if inner.status == PlayerStatus::Stopped {
                    return;
                }
                inner.generation += 1;
                match inner.queue.pop_front() {
                    Some(item) => {
                        inner.current = Some(item.clone());
                        inner.status = PlayerStatus::Loading;
                        self.publish_player(&inner);
                        self.publish_queue(&inner);
                        (item, inner.generation, Self::protected_ids(&inner))
                    }
                    None => {
                        self.engine.stop();
                        inner.current = None;
                        inner.status = PlayerStatus::Idle;
                        self.publish_player(&inner);
                        self.publish_queue(&inner);
                        return;
                    }
                }
            };

            let video_id = item.track.playback_video_id().to_owned();
            let fetched = self.cache.fetch(&video_id, &protect).await;

            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return; // superseded by skip/stop/another advance
            }
            match fetched {
                Ok(path) => {
                    self.engine.load(path, generation, 0, false);
                    inner.status = PlayerStatus::Playing;
                    self.publish_player(&inner);
                    drop(inner);
                    self.spawn_prefetch();
                    return;
                }
                Err(err) => {
                    drop(inner);
                    self.bus.notice(NoticeLevel::Error, format!("Couldn't load \"{}\": {err}", item.track.display_name()));
                }
            }
        }
    }

    async fn on_engine_event(self: &Arc<Self>, event: EngineEvent) {
        match event {
            EngineEvent::Ended { generation } => {
                let finished = {
                    let mut inner = self.inner.lock().await;
                    if inner.generation != generation || inner.status != PlayerStatus::Playing {
                        return;
                    }
                    inner.current.take()
                };
                if let Some(item) = finished {
                    self.record_history(item, false);
                }
                self.advance().await;
            }
            EngineEvent::Failed { generation, message } => {
                let failed = {
                    let mut inner = self.inner.lock().await;
                    if inner.generation != generation {
                        return;
                    }
                    inner.current.take()
                };
                if let Some(item) = failed {
                    self.cache.evict(item.track.playback_video_id());
                    self.bus.notice(NoticeLevel::Error, format!("Playback failed for \"{}\": {message}", item.track.display_name()));
                }
                self.advance().await;
            }
        }
    }

    /// Persists the queue synchronously and stops audio. Called on app exit.
    pub fn shutdown(&self) {
        let items: Vec<QueueItem> = {
            let inner = tauri::async_runtime::block_on(self.inner.lock());
            inner.current.iter().chain(inner.queue.iter()).cloned().collect()
        };
        if let Err(err) = self.db.save_queue_blocking(&items) {
            tracing::error!(%err, "failed to persist queue on exit");
        }
        self.engine.shutdown();
    }
}

/// Coalesces queue writes: only the latest snapshot is ever written.
async fn persist_loop(db: Database, mut rx: watch::Receiver<Vec<QueueItem>>) {
    while rx.changed().await.is_ok() {
        let items = rx.borrow_and_update().clone();
        if let Err(err) = db.save_queue(items).await {
            tracing::error!(%err, "failed to persist queue");
        }
    }
}

async fn engine_event_loop(ctrl: std::sync::Weak<PlaybackController>, mut rx: mpsc::UnboundedReceiver<EngineEvent>) {
    while let Some(event) = rx.recv().await {
        let Some(ctrl) = ctrl.upgrade() else { return };
        ctrl.on_engine_event(event).await;
    }
}

/// Applies settings that affect the audio engine as soon as they change.
async fn settings_loop(
    ctrl: std::sync::Weak<PlaybackController>,
    mut rx: broadcast::Receiver<AppEvent>,
    mut device: Option<String>,
) {
    loop {
        let settings = match rx.recv().await {
            Ok(AppEvent::Settings(s)) => s,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(ctrl) = ctrl.upgrade() else { return };
        ctrl.engine.set_volume(settings.volume);
        if settings.output_device != device {
            device = settings.output_device.clone();
            ctrl.engine.set_device(device.clone());
        }
        let inner = ctrl.inner.lock().await;
        ctrl.publish_player(&inner);
    }
}
