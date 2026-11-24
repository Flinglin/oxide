use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    REMOTE_DESKTOP_MAX_FRAME_UPDATE_BATCH_REGIONS, RemoteDesktopFrameFormat,
    RemoteDesktopHelperEvent, RemoteDesktopSize,
};

const DEFAULT_MAX_EVENTS: usize = 256;
const DEFAULT_MAX_DIRTY_BYTES: usize = 32 * 1024 * 1024;
const MAX_BASE_FRAME_BYTES: usize = RemoteDesktopSize::MAX_DIMENSION as usize
    * RemoteDesktopSize::MAX_DIMENSION as usize
    * RemoteDesktopFrameFormat::Rgba8.bytes_per_pixel();
const FRAME_PRESENTATION_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteDesktopFrameQueuePush {
    Queued,
    RecoveryRequired,
    AwaitingRecovery,
    Discarded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteDesktopFrameDeliveryDecision {
    pub frame_ready: bool,
    pub recovery_required: bool,
}

pub struct RemoteDesktopFrameQueue {
    frames: VecDeque<RemoteDesktopHelperEvent>,
    queued_dirty_bytes: usize,
    active_graphics_epoch: Option<u64>,
    awaiting_base_frame: bool,
    max_events: usize,
    max_dirty_bytes: usize,
}

impl Default for RemoteDesktopFrameQueue {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_EVENTS, DEFAULT_MAX_DIRTY_BYTES)
    }
}

impl RemoteDesktopFrameQueue {
    pub fn with_limits(max_events: usize, max_dirty_bytes: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            queued_dirty_bytes: 0,
            active_graphics_epoch: None,
            awaiting_base_frame: false,
            max_events,
            max_dirty_bytes,
        }
    }

    pub fn push(&mut self, event: RemoteDesktopHelperEvent) -> RemoteDesktopFrameQueuePush {
        if let RemoteDesktopHelperEvent::FrameStreamReset { graphics_epoch } = &event {
            let graphics_epoch = *graphics_epoch;
            if self
                .active_graphics_epoch
                .is_some_and(|active_epoch| graphics_epoch <= active_epoch)
            {
                return RemoteDesktopFrameQueuePush::Discarded;
            }
            // A reset starts a new graphics lineage while the currently
            // presented frame remains available until its replacement arrives.
            self.frames.clear();
            self.queued_dirty_bytes = 0;
            self.active_graphics_epoch = Some(graphics_epoch);
            self.awaiting_base_frame = true;
            self.frames
                .push_back(RemoteDesktopHelperEvent::FrameStreamReset { graphics_epoch });
            return RemoteDesktopFrameQueuePush::Queued;
        }

        if let RemoteDesktopHelperEvent::Frame { frame } = &event {
            if self
                .active_graphics_epoch
                .is_some_and(|active_epoch| frame.graphics_epoch < active_epoch)
            {
                return RemoteDesktopFrameQueuePush::Discarded;
            }
            let event_bytes = frame_event_bytes(&event);
            self.frames.clear();
            self.queued_dirty_bytes = 0;
            if event_bytes > MAX_BASE_FRAME_BYTES {
                self.awaiting_base_frame = true;
                return RemoteDesktopFrameQueuePush::RecoveryRequired;
            }
            self.active_graphics_epoch = Some(frame.graphics_epoch);
            self.frames.push_back(event);
            self.awaiting_base_frame = false;
            return RemoteDesktopFrameQueuePush::Queued;
        }

        if let RemoteDesktopHelperEvent::FrameUpdate { update } = &event {
            match self.active_graphics_epoch {
                Some(active_epoch) if update.graphics_epoch < active_epoch => {
                    return RemoteDesktopFrameQueuePush::Discarded;
                }
                Some(active_epoch) if update.graphics_epoch > active_epoch => {
                    self.frames.clear();
                    self.queued_dirty_bytes = 0;
                    self.active_graphics_epoch = Some(update.graphics_epoch);
                    self.awaiting_base_frame = true;
                    return RemoteDesktopFrameQueuePush::RecoveryRequired;
                }
                None => self.active_graphics_epoch = Some(update.graphics_epoch),
                _ => {}
            }
        }
        if let RemoteDesktopHelperEvent::FrameUpdateBatch { batch } = &event {
            let Some(graphics_epoch) = batch.graphics_epoch() else {
                self.awaiting_base_frame = true;
                return RemoteDesktopFrameQueuePush::RecoveryRequired;
            };
            match self.active_graphics_epoch {
                Some(active_epoch) if graphics_epoch < active_epoch => {
                    return RemoteDesktopFrameQueuePush::Discarded;
                }
                Some(active_epoch) if graphics_epoch > active_epoch => {
                    self.frames.clear();
                    self.queued_dirty_bytes = 0;
                    self.active_graphics_epoch = Some(graphics_epoch);
                    self.awaiting_base_frame = true;
                    return RemoteDesktopFrameQueuePush::RecoveryRequired;
                }
                None => self.active_graphics_epoch = Some(graphics_epoch),
                _ => {}
            }
        }

        if self.awaiting_base_frame {
            // Deltas after a dropped predecessor cannot be applied safely.
            return RemoteDesktopFrameQueuePush::AwaitingRecovery;
        }

        if let Some(existing) = self.frames.back_mut() {
            if let Some(incoming) = merge_frame_event(existing, event) {
                self.frames.push_back(incoming);
            }
        } else {
            self.frames.push_back(event);
        }
        self.queued_dirty_bytes = self
            .frames
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    RemoteDesktopHelperEvent::FrameUpdate { .. }
                        | RemoteDesktopHelperEvent::FrameUpdateBatch { .. }
                )
            })
            .map(frame_event_bytes)
            .fold(0_usize, usize::saturating_add);
        if self.frames.len() <= self.max_events && self.queued_dirty_bytes <= self.max_dirty_bytes {
            return RemoteDesktopFrameQueuePush::Queued;
        }

        // Keep the newest recoverable base while discarding a broken delta tail.
        let recoverable_frame = self
            .frames
            .iter()
            .rposition(|event| matches!(event, RemoteDesktopHelperEvent::Frame { .. }))
            .and_then(|index| self.frames.remove(index));
        self.frames.clear();
        self.queued_dirty_bytes = 0;
        if let Some(frame) = recoverable_frame {
            self.frames.push_back(frame);
        }
        self.awaiting_base_frame = true;
        RemoteDesktopFrameQueuePush::RecoveryRequired
    }

    fn retain_latest_frame_only(&mut self) -> bool {
        if self.frames.is_empty()
            || (self.frames.len() == 1
                && self
                    .frames
                    .front()
                    .is_some_and(|event| matches!(event, RemoteDesktopHelperEvent::Frame { .. })))
        {
            return false;
        }

        // A hidden surface cannot safely preserve an unbounded sparse-delta
        // history. Drop it and request one new base frame from the live worker.
        self.frames.clear();
        self.queued_dirty_bytes = 0;
        self.awaiting_base_frame = true;
        true
    }

    pub fn pop_front(&mut self) -> Option<RemoteDesktopHelperEvent> {
        let event = self.frames.pop_front()?;
        if matches!(
            event,
            RemoteDesktopHelperEvent::FrameUpdate { .. }
                | RemoteDesktopHelperEvent::FrameUpdateBatch { .. }
        ) {
            self.queued_dirty_bytes = self
                .queued_dirty_bytes
                .saturating_sub(frame_event_bytes(&event));
        }
        Some(event)
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[derive(Clone)]
pub struct RemoteDesktopFrameDeliverySlot {
    queue: Arc<Mutex<RemoteDesktopFrameQueue>>,
    ready_queued: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    last_presented_at: Arc<Mutex<Option<Instant>>>,
}

impl Default for RemoteDesktopFrameDeliverySlot {
    fn default() -> Self {
        Self {
            queue: Arc::new(Mutex::new(RemoteDesktopFrameQueue::default())),
            ready_queued: Arc::new(AtomicBool::new(false)),
            visible: Arc::new(AtomicBool::new(true)),
            last_presented_at: Arc::new(Mutex::new(None)),
        }
    }
}

impl RemoteDesktopFrameDeliverySlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, event: RemoteDesktopHelperEvent) -> RemoteDesktopFrameDeliveryDecision {
        let queue_push = {
            let Ok(mut queue) = self.queue.lock() else {
                return RemoteDesktopFrameDeliveryDecision::default();
            };
            let queue_push = queue.push(event);
            if !self.is_visible() && queue.retain_latest_frame_only() {
                RemoteDesktopFrameQueuePush::RecoveryRequired
            } else {
                queue_push
            }
        };

        let recovery_required = queue_push == RemoteDesktopFrameQueuePush::RecoveryRequired;
        let recovery_has_no_frame = recovery_required && !self.has_queued_frame_events();
        if recovery_has_no_frame {
            // The recovery base must be able to publish a fresh ready notice.
            self.ready_queued.store(false, Ordering::Release);
        }
        if queue_push == RemoteDesktopFrameQueuePush::AwaitingRecovery || recovery_has_no_frame {
            return RemoteDesktopFrameDeliveryDecision {
                frame_ready: false,
                recovery_required,
            };
        }
        if queue_push == RemoteDesktopFrameQueuePush::Discarded {
            return RemoteDesktopFrameDeliveryDecision::default();
        }

        RemoteDesktopFrameDeliveryDecision {
            frame_ready: self.mark_frame_ready_queued(),
            recovery_required,
        }
    }

    pub fn take(&self) -> Option<RemoteDesktopHelperEvent> {
        self.queue.lock().ok()?.pop_front()
    }

    pub fn has_queued_frame_events(&self) -> bool {
        self.queue
            .lock()
            .map(|queue| !queue.is_empty())
            .unwrap_or(false)
    }

    pub fn set_visible(&self, visible: bool) -> bool {
        self.visible.store(visible, Ordering::Release);
        let recovery_required = if visible {
            false
        } else {
            self.queue
                .lock()
                .map(|mut queue| queue.retain_latest_frame_only())
                .unwrap_or(false)
        };
        if !self.has_queued_frame_events() {
            // A dropped hidden delta tail must not suppress the next base-frame wake.
            self.ready_queued.store(false, Ordering::Release);
        }
        recovery_required
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    pub fn complete_delivery(&self) -> bool {
        self.ready_queued.store(false, Ordering::Release);
        self.has_queued_frame_events()
    }

    pub fn mark_frame_ready_queued(&self) -> bool {
        !self.ready_queued.swap(true, Ordering::AcqRel)
    }

    pub fn mark_frame_presented(&self) {
        if let Ok(mut last_presented_at) = self.last_presented_at.lock() {
            *last_presented_at = Some(Instant::now());
        }
    }

    pub fn next_frame_ready_delay(&self) -> Duration {
        let now = Instant::now();
        let Ok(last_presented_at) = self.last_presented_at.lock() else {
            return Duration::ZERO;
        };
        let Some(previous_presented_at) = *last_presented_at else {
            return Duration::ZERO;
        };
        FRAME_PRESENTATION_INTERVAL
            .saturating_sub(now.saturating_duration_since(previous_presented_at))
    }
}

pub fn is_remote_desktop_frame_event(event: &RemoteDesktopHelperEvent) -> bool {
    matches!(
        event,
        RemoteDesktopHelperEvent::Frame { .. }
            | RemoteDesktopHelperEvent::FrameUpdate { .. }
            | RemoteDesktopHelperEvent::FrameUpdateBatch { .. }
            | RemoteDesktopHelperEvent::FrameStreamReset { .. }
    )
}

fn frame_event_bytes(event: &RemoteDesktopHelperEvent) -> usize {
    match event {
        RemoteDesktopHelperEvent::Frame { frame } => frame.bytes.len(),
        RemoteDesktopHelperEvent::FrameUpdate { update } => update.bytes.len(),
        RemoteDesktopHelperEvent::FrameUpdateBatch { batch } => batch.byte_len(),
        RemoteDesktopHelperEvent::FrameStreamReset { .. } => 0,
        _ => 0,
    }
}

// Return a frame that still needs its own queue slot when merging is not possible.
fn merge_frame_event(
    existing: &mut RemoteDesktopHelperEvent,
    incoming: RemoteDesktopHelperEvent,
) -> Option<RemoteDesktopHelperEvent> {
    match existing {
        RemoteDesktopHelperEvent::Frame { frame } => match incoming {
            RemoteDesktopHelperEvent::FrameUpdate { update } => {
                if !frame.apply_update(&update) {
                    return Some(RemoteDesktopHelperEvent::FrameUpdate { update });
                }
            }
            RemoteDesktopHelperEvent::FrameUpdateBatch { batch } => {
                if !frame.apply_update_batch(&batch) {
                    return Some(RemoteDesktopHelperEvent::FrameUpdateBatch { batch });
                }
            }
            incoming => *existing = incoming,
        },
        RemoteDesktopHelperEvent::FrameUpdate { update } => match incoming {
            RemoteDesktopHelperEvent::FrameUpdate {
                update: incoming_update,
            } => {
                if !update.merge(&incoming_update) {
                    return Some(RemoteDesktopHelperEvent::FrameUpdate {
                        update: incoming_update,
                    });
                }
            }
            incoming @ RemoteDesktopHelperEvent::FrameUpdateBatch { .. } => {
                return Some(incoming);
            }
            incoming => *existing = incoming,
        },
        RemoteDesktopHelperEvent::FrameUpdateBatch { batch } => match incoming {
            RemoteDesktopHelperEvent::FrameUpdateBatch {
                batch: incoming_batch,
            } => {
                if let Err(incoming_batch) = batch.merge(
                    incoming_batch,
                    REMOTE_DESKTOP_MAX_FRAME_UPDATE_BATCH_REGIONS,
                ) {
                    return Some(RemoteDesktopHelperEvent::FrameUpdateBatch {
                        batch: incoming_batch,
                    });
                }
            }
            incoming => return Some(incoming),
        },
        slot => *slot = incoming,
    }
    None
}
