use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::Instant,
};

use gpui::{Context, EventEmitter, Task, Window};
use oxide_ssh_launch::{NativeConnectionHandoff, NativeConnectionLaunch};
use tokio::sync::Notify;

use super::{WorkspaceApp, delivery};

/// A window-scoped action emitted by platform integrations.
pub(in crate::workspace) enum WindowIntentAction {
    ShowMainWindow,
    HideMainWindow,
    NewConnection,
    OpenSettings,
    CheckForUpdates,
    Quit,
    OpenNativeConnection(NativeConnectionHandoff),
    OpenExternalConnectionUri(NativeConnectionLaunch),
}

/// Moves a platform action into the root window adapter exactly once.
pub(in crate::workspace) struct WindowIntent {
    action: RefCell<Option<WindowIntentAction>>,
}

impl WindowIntent {
    fn new(action: WindowIntentAction) -> Self {
        Self {
            action: RefCell::new(Some(action)),
        }
    }

    pub(in crate::workspace) fn take(&self) -> Option<WindowIntentAction> {
        self.action.borrow_mut().take()
    }
}

enum DesktopPresenceSourceReceiver {
    Platform(oxide_desktop_presence::DesktopPresenceReceiver),
}

struct DesktopPresenceSource {
    receiver: DesktopPresenceSourceReceiver,
    notification: Arc<Notify>,
}

impl DesktopPresenceSource {
    fn platform(receiver: oxide_desktop_presence::DesktopPresenceReceiver) -> Self {
        let notification = receiver.notification();
        Self {
            receiver: DesktopPresenceSourceReceiver::Platform(receiver),
            notification,
        }
    }

    fn drain(&self) -> delivery::ChannelDrain<oxide_desktop_presence::DesktopPresenceEvent> {
        match &self.receiver {
            DesktopPresenceSourceReceiver::Platform(receiver) => drain_with(|| receiver.try_recv()),
        }
    }
}

enum SingleInstanceSourceReceiver {
    Platform(crate::single_instance::SingleInstanceReceiver),
}

struct SingleInstanceSource {
    receiver: SingleInstanceSourceReceiver,
    notification: Arc<Notify>,
}

impl SingleInstanceSource {
    fn platform(receiver: crate::single_instance::SingleInstanceReceiver) -> Self {
        let notification = receiver.notification();
        Self {
            receiver: SingleInstanceSourceReceiver::Platform(receiver),
            notification,
        }
    }

    fn drain(&self) -> delivery::ChannelDrain<crate::single_instance::SingleInstanceEvent> {
        match &self.receiver {
            SingleInstanceSourceReceiver::Platform(receiver) => {
                // Release the shared application receiver before emitting any intent.
                let receiver = receiver.lock().expect("single-instance receiver poisoned");
                drain_with(|| receiver.try_recv())
            }
        }
    }
}

/// Stops one external-source waiter even when its platform sender remains alive.
#[derive(Clone)]
struct WindowIntentWaiterStop {
    stopped: Arc<AtomicBool>,
    notification: Arc<Notify>,
}

impl Default for WindowIntentWaiterStop {
    fn default() -> Self {
        Self {
            stopped: Arc::new(AtomicBool::new(false)),
            notification: Arc::new(Notify::new()),
        }
    }
}

impl WindowIntentWaiterStop {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.notification.notify_one();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        self.notification.notified().await;
    }
}

/// Owns platform window-intent receivers and their foreground delivery lifetime.
pub(in crate::workspace) struct WorkspaceWindowIntentEntity {
    desktop_presence: Option<DesktopPresenceSource>,
    single_instance: Option<SingleInstanceSource>,
    _desktop_presence_task: Option<Task<()>>,
    _single_instance_task: Option<Task<()>>,
}

impl EventEmitter<WindowIntent> for WorkspaceWindowIntentEntity {}

impl WorkspaceWindowIntentEntity {
    pub(in crate::workspace) fn new(
        desktop_presence_receiver: Option<oxide_desktop_presence::DesktopPresenceReceiver>,
        single_instance_receiver: Option<crate::single_instance::SingleInstanceReceiver>,
        cx: &mut Context<Self>,
    ) -> Self {
        let desktop_presence = desktop_presence_receiver.map(DesktopPresenceSource::platform);
        let single_instance = single_instance_receiver.map(SingleInstanceSource::platform);
        Self::new_with_sources(desktop_presence, single_instance, cx)
    }

    fn new_with_sources(
        desktop_presence: Option<DesktopPresenceSource>,
        single_instance: Option<SingleInstanceSource>,
        cx: &mut Context<Self>,
    ) -> Self {
        let desktop_presence_waiter_stop = WindowIntentWaiterStop::default();
        let single_instance_waiter_stop = WindowIntentWaiterStop::default();
        let desktop_presence_task = desktop_presence.as_ref().map(|source| {
            Self::spawn_desktop_presence_waiter(
                source.notification.clone(),
                desktop_presence_waiter_stop.clone(),
                cx,
            )
        });
        let single_instance_task = single_instance.as_ref().map(|source| {
            Self::spawn_single_instance_waiter(
                source.notification.clone(),
                single_instance_waiter_stop.clone(),
                cx,
            )
        });

        Self {
            desktop_presence,
            single_instance,
            _desktop_presence_task: desktop_presence_task,
            _single_instance_task: single_instance_task,
        }
    }

    fn spawn_desktop_presence_waiter(
        source_notification: Arc<Notify>,
        waiter_stop: WindowIntentWaiterStop,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let release_stop = waiter_stop.clone();
        cx.on_release(move |_, _| {
            release_stop.stop();
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            loop {
                tokio::select! {
                    _ = source_notification.notified() => {}
                    _ = waiter_stop.wait() => {}
                }
                if waiter_stop.is_stopped() {
                    break;
                }
                let Some(backlog_remaining) = entity
                    .update(cx, |entity, cx| entity.drain_desktop_presence(cx))
                    .unwrap_or(None)
                else {
                    break;
                };
                if backlog_remaining {
                    source_notification.notify_one();
                }
            }
        })
    }

    fn spawn_single_instance_waiter(
        source_notification: Arc<Notify>,
        waiter_stop: WindowIntentWaiterStop,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let release_stop = waiter_stop.clone();
        cx.on_release(move |_, _| {
            release_stop.stop();
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            loop {
                tokio::select! {
                    _ = source_notification.notified() => {}
                    _ = waiter_stop.wait() => {}
                }
                if waiter_stop.is_stopped() {
                    break;
                }
                let Some(backlog_remaining) = entity
                    .update(cx, |entity, cx| entity.drain_single_instance(cx))
                    .unwrap_or(None)
                else {
                    break;
                };
                if backlog_remaining {
                    source_notification.notify_one();
                }
            }
        })
    }

    fn drain_desktop_presence(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        let Some(source) = self.desktop_presence.as_ref() else {
            return None;
        };
        let batch = source.drain();
        for event in batch.items {
            let action = match event {
                oxide_desktop_presence::DesktopPresenceEvent::ShowMainWindow => {
                    WindowIntentAction::ShowMainWindow
                }
                oxide_desktop_presence::DesktopPresenceEvent::HideMainWindow => {
                    WindowIntentAction::HideMainWindow
                }
                oxide_desktop_presence::DesktopPresenceEvent::NewConnection => {
                    WindowIntentAction::NewConnection
                }
                oxide_desktop_presence::DesktopPresenceEvent::OpenSettings => {
                    WindowIntentAction::OpenSettings
                }
                oxide_desktop_presence::DesktopPresenceEvent::CheckForUpdates => {
                    WindowIntentAction::CheckForUpdates
                }
                oxide_desktop_presence::DesktopPresenceEvent::Quit => WindowIntentAction::Quit,
            };
            cx.emit(WindowIntent::new(action));
        }
        if batch.disconnected {
            self.desktop_presence = None;
            return None;
        }
        Some(batch.outcome.backlog_remaining)
    }

    fn drain_single_instance(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        let Some(source) = self.single_instance.as_ref() else {
            return None;
        };
        let batch = source.drain();
        for event in batch.items {
            let action = match event {
                crate::single_instance::SingleInstanceEvent::ShowMainWindow => {
                    WindowIntentAction::ShowMainWindow
                }
                crate::single_instance::SingleInstanceEvent::OpenNativeConnection(launch) => {
                    WindowIntentAction::OpenNativeConnection(launch)
                }
                crate::single_instance::SingleInstanceEvent::OpenExternalConnectionUri(launch) => {
                    WindowIntentAction::OpenExternalConnectionUri(launch)
                }
            };
            cx.emit(WindowIntent::new(action));
        }
        if batch.disconnected {
            self.single_instance = None;
            return None;
        }
        Some(batch.outcome.backlog_remaining)
    }
}

fn drain_with<T>(
    mut receive: impl FnMut() -> Result<T, TryRecvError>,
) -> delivery::ChannelDrain<T> {
    let started_at = Instant::now();
    let mut items = Vec::new();
    let mut source_exhausted = false;
    let mut disconnected = false;
    while delivery::USER_ACTION_DELIVERY_BUDGET.allows_next(items.len(), started_at.elapsed()) {
        match receive() {
            Ok(item) => items.push(item),
            Err(TryRecvError::Empty) => {
                source_exhausted = true;
                break;
            }
            Err(TryRecvError::Disconnected) => {
                source_exhausted = true;
                disconnected = true;
                break;
            }
        }
    }
    delivery::ChannelDrain {
        outcome: delivery::USER_ACTION_DELIVERY_BUDGET.outcome(
            items.len(),
            started_at.elapsed(),
            source_exhausted,
        ),
        items,
        disconnected,
    }
}

impl WorkspaceApp {
    pub(in crate::workspace) fn handle_window_intent(
        &mut self,
        action: WindowIntentAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            WindowIntentAction::ShowMainWindow => {
                oxide_desktop_presence::show_main_window();
            }
            WindowIntentAction::HideMainWindow => {
                oxide_desktop_presence::hide_main_window();
            }
            WindowIntentAction::NewConnection => {
                oxide_desktop_presence::show_main_window();
                cx.dispatch_action(&crate::NewConnection);
            }
            WindowIntentAction::OpenSettings => {
                oxide_desktop_presence::show_main_window();
                cx.dispatch_action(&crate::OpenSettings);
            }
            WindowIntentAction::CheckForUpdates => {
                oxide_desktop_presence::show_main_window();
                cx.dispatch_action(&crate::OpenSettings);
                self.check_native_update(cx);
            }
            WindowIntentAction::Quit => {
                // Tray events may arrive while every window is hidden, so they cannot rely on
                // focused action bubbling to protect a dirty Knowledge draft.
                super::request_app_quit(cx);
            }
            WindowIntentAction::OpenNativeConnection(launch) => {
                oxide_desktop_presence::show_main_window();
                if let Err(error) = self.open_native_connection_handoff(launch, window, cx) {
                    eprintln!("failed to open forwarded connection launch: {error:#}");
                }
                window.activate_window();
            }
            WindowIntentAction::OpenExternalConnectionUri(launch) => {
                if !self
                    .settings_store
                    .settings()
                    .general
                    .external_connection_uris_enabled
                {
                    return;
                }
                oxide_desktop_presence::show_main_window();
                if let Err(error) = self.open_native_connection_launch(launch, window, cx) {
                    eprintln!("failed to open external connection URI: {error:#}");
                }
                window.activate_window();
            }
        }
    }
}
