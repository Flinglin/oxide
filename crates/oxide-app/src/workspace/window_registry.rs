use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    rc::Rc,
    time::Instant,
};

use gpui::{AnyWindowHandle, AppContext, Context, Window, WindowId};

use super::*;

/// Identifies the UI role owned by one native workspace window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum WindowRole {
    Main,
    Detached { tab_id: TabId },
}

/// Guards a window registration against delayed release callbacks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::workspace) struct WindowRegistration {
    generation: u64,
}

/// A registry event produced only by an active-window count transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum WindowRegistryEvent {
    LastWindowReleased,
}

#[derive(Clone, Copy)]
struct RegisteredWindow<Handle> {
    role: WindowRole,
    window_id: WindowId,
    handle: Handle,
}

/// Carries one effect and the concrete window selected for this attempt.
struct WindowDelivery<Handle, Effect, CoalescingKey> {
    registration: WindowRegistration,
    window_id: WindowId,
    handle: Handle,
    effect: Effect,
    coalescing_key: Option<CoalescingKey>,
    target_hint: WindowTargetHint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowTargetHint {
    MainOrAny,
    Prefer(WindowId),
}

/// Selects live window targets and retains reliable effects while none exist.
pub(in crate::workspace) struct WindowRegistry<Handle, Effect, CoalescingKey> {
    next_generation: u64,
    reservations: HashMap<WindowRegistration, WindowRole>,
    windows: BTreeMap<WindowRegistration, RegisteredWindow<Handle>>,
    pending_effects: VecDeque<(Effect, Option<CoalescingKey>, WindowTargetHint)>,
    pending_coalescing_keys: HashSet<CoalescingKey>,
    events: VecDeque<WindowRegistryEvent>,
}

impl<Handle, Effect, CoalescingKey> Default for WindowRegistry<Handle, Effect, CoalescingKey> {
    fn default() -> Self {
        Self {
            next_generation: 0,
            reservations: HashMap::new(),
            windows: BTreeMap::new(),
            pending_effects: VecDeque::new(),
            pending_coalescing_keys: HashSet::new(),
            events: VecDeque::new(),
        }
    }
}

impl<Handle, Effect, CoalescingKey> WindowRegistry<Handle, Effect, CoalescingKey>
where
    Handle: std::marker::Copy,
    CoalescingKey: std::marker::Copy + Eq + std::hash::Hash,
{
    fn reserve(&mut self, role: WindowRole) -> WindowRegistration {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("workspace window generation exhausted");
        let registration = WindowRegistration {
            generation: self.next_generation,
        };
        self.reservations.insert(registration, role);
        registration
    }

    fn commit(
        &mut self,
        registration: WindowRegistration,
        window_id: WindowId,
        handle: Handle,
    ) -> bool {
        let Some(role) = self.reservations.remove(&registration) else {
            return false;
        };
        self.windows.insert(
            registration,
            RegisteredWindow {
                role,
                window_id,
                handle,
            },
        );
        true
    }

    fn rollback(&mut self, registration: WindowRegistration) -> bool {
        self.reservations.remove(&registration).is_some()
    }

    pub(in crate::workspace) fn release(
        &mut self,
        registration: WindowRegistration,
        window_id: WindowId,
    ) -> bool {
        let Some(window) = self.windows.get(&registration) else {
            return false;
        };
        if window.window_id != window_id {
            return false;
        }
        self.windows.remove(&registration);
        if self.windows.is_empty() {
            // Only the successful non-empty-to-empty transition emits this event.
            self.events
                .push_back(WindowRegistryEvent::LastWindowReleased);
        }
        true
    }

    fn enqueue(
        &mut self,
        effect: Effect,
        coalescing_key: Option<CoalescingKey>,
        target_hint: WindowTargetHint,
    ) {
        if coalescing_key.is_some_and(|key| !self.pending_coalescing_keys.insert(key)) {
            return;
        }
        self.pending_effects
            .push_back((effect, coalescing_key, target_hint));
    }

    fn requeue_front(
        &mut self,
        effect: Effect,
        coalescing_key: Option<CoalescingKey>,
        target_hint: WindowTargetHint,
    ) {
        if coalescing_key.is_some_and(|key| !self.pending_coalescing_keys.insert(key)) {
            return;
        }
        self.pending_effects
            .push_front((effect, coalescing_key, target_hint));
    }

    fn next_delivery(&mut self) -> Option<WindowDelivery<Handle, Effect, CoalescingKey>> {
        let target_hint = self.pending_effects.front()?.2;
        let preferred = match target_hint {
            WindowTargetHint::MainOrAny => None,
            WindowTargetHint::Prefer(window_id) => self
                .windows
                .iter()
                .find(|(_, window)| window.window_id == window_id),
        };
        let target = preferred
            .or_else(|| {
                self.windows
                    .iter()
                    .find(|(_, window)| window.role == WindowRole::Main)
            })
            .or_else(|| self.windows.iter().next_back())
            .map(|(registration, window)| (*registration, *window))?;
        let (effect, coalescing_key, target_hint) = self.pending_effects.pop_front()?;
        if let Some(key) = coalescing_key {
            self.pending_coalescing_keys.remove(&key);
        }
        Some(WindowDelivery {
            registration: target.0,
            window_id: target.1.window_id,
            handle: target.1.handle,
            effect,
            coalescing_key,
            target_hint,
        })
    }

    pub(in crate::workspace) fn handle_for_role(&self, role: WindowRole) -> Option<Handle> {
        self.windows
            .values()
            .find(|window| window.role == role)
            .map(|window| window.handle)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) enum PluginWindowEffect {
    ManagerDeliveryReady,
    RuntimeRequestsReady,
    RuntimeSubscriptionSampleDue,
    RuntimeIntentsReady,
    OxideImportIntentsReady,
}

impl From<&plugin_entity::PluginWorkspaceEvent> for PluginWindowEffect {
    fn from(event: &plugin_entity::PluginWorkspaceEvent) -> Self {
        match event {
            plugin_entity::PluginWorkspaceEvent::ManagerDeliveryReady => Self::ManagerDeliveryReady,
            plugin_entity::PluginWorkspaceEvent::RuntimeRequestsReady => Self::RuntimeRequestsReady,
            plugin_entity::PluginWorkspaceEvent::RuntimeSubscriptionSampleDue => {
                Self::RuntimeSubscriptionSampleDue
            }
            plugin_entity::PluginWorkspaceEvent::RuntimeIntentsReady => Self::RuntimeIntentsReady,
            plugin_entity::PluginWorkspaceEvent::OxideImportIntentsReady => {
                Self::OxideImportIntentsReady
            }
        }
    }
}

impl PluginWindowEffect {
    fn into_event(self) -> plugin_entity::PluginWorkspaceEvent {
        match self {
            Self::ManagerDeliveryReady => plugin_entity::PluginWorkspaceEvent::ManagerDeliveryReady,
            Self::RuntimeRequestsReady => plugin_entity::PluginWorkspaceEvent::RuntimeRequestsReady,
            Self::RuntimeSubscriptionSampleDue => {
                plugin_entity::PluginWorkspaceEvent::RuntimeSubscriptionSampleDue
            }
            Self::RuntimeIntentsReady => plugin_entity::PluginWorkspaceEvent::RuntimeIntentsReady,
            Self::OxideImportIntentsReady => {
                plugin_entity::PluginWorkspaceEvent::OxideImportIntentsReady
            }
        }
    }
}

/// A typed root adapter effect retained until a native window can apply it.
pub(in crate::workspace) enum WorkspaceWindowEffect {
    WindowIntent(window_intent::WindowIntentAction),
    Runtime(runtime_entity::WorkspaceRuntimeEvent),
    ConnectionFlow,
    CloudSync(cloud_sync::CloudSyncWorkspaceEvent),
    Plugin(PluginWindowEffect),
    TabHost(tabs::WorkspaceTabHostEvent),
    Graphics,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) enum WorkspaceWindowEffectKey {
    Runtime,
    ConnectionFlow,
    CloudSyncDeliveries,
    Plugin(PluginWindowEffect),
    TabCloseProcessCheck,
    Graphics,
}

impl WorkspaceWindowEffect {
    fn coalescing_key(&self) -> Option<WorkspaceWindowEffectKey> {
        match self {
            Self::WindowIntent(_) => None,
            Self::Runtime(_) => Some(WorkspaceWindowEffectKey::Runtime),
            Self::ConnectionFlow => Some(WorkspaceWindowEffectKey::ConnectionFlow),
            Self::CloudSync(cloud_sync::CloudSyncWorkspaceEvent::DeliveriesReady) => {
                Some(WorkspaceWindowEffectKey::CloudSyncDeliveries)
            }
            Self::CloudSync(_) => None,
            Self::Plugin(effect) => Some(WorkspaceWindowEffectKey::Plugin(*effect)),
            Self::TabHost(tabs::WorkspaceTabHostEvent::CloseProcessCheckReady) => {
                Some(WorkspaceWindowEffectKey::TabCloseProcessCheck)
            }
            Self::TabHost(_) => None,
            Self::Graphics => Some(WorkspaceWindowEffectKey::Graphics),
        }
    }

    fn target_hint(&self) -> WindowTargetHint {
        match self {
            Self::CloudSync(cloud_sync::CloudSyncWorkspaceEvent::UiIntent(intent)) => {
                let source_window = match intent {
                    cloud_sync::CloudSyncUiIntent::BeginInputSelection {
                        source_window, ..
                    }
                    | cloud_sync::CloudSyncUiIntent::UpdateInputSelection {
                        source_window, ..
                    }
                    | cloud_sync::CloudSyncUiIntent::UpdateInputAnchor { source_window, .. }
                    | cloud_sync::CloudSyncUiIntent::UpdateSelectAnchor { source_window, .. } => {
                        Some(*source_window)
                    }
                    _ => None,
                };
                source_window.map_or(WindowTargetHint::MainOrAny, |handle| {
                    WindowTargetHint::Prefer(handle.window_id())
                })
            }
            Self::TabHost(tabs::WorkspaceTabHostEvent::TerminalPaneDelivery {
                window_handle,
                ..
            }) => WindowTargetHint::Prefer(window_handle.window_id()),
            _ => WindowTargetHint::MainOrAny,
        }
    }
}

pub(in crate::workspace) type WorkspaceWindowRegistry =
    WindowRegistry<AnyWindowHandle, WorkspaceWindowEffect, WorkspaceWindowEffectKey>;

impl WorkspaceApp {
    pub(in crate::workspace) fn reserve_workspace_window(
        &mut self,
        role: WindowRole,
    ) -> WindowRegistration {
        self.window_registry.reserve(role)
    }

    pub(in crate::workspace) fn commit_workspace_window(
        &mut self,
        registration: WindowRegistration,
        handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> bool {
        let committed = self
            .window_registry
            .commit(registration, handle.window_id(), handle);
        if committed {
            self.schedule_window_effect_delivery(cx);
        }
        committed
    }

    pub(in crate::workspace) fn rollback_workspace_window(
        &mut self,
        registration: WindowRegistration,
    ) -> bool {
        self.window_registry.rollback(registration)
    }

    pub(in crate::workspace) fn release_workspace_window(
        &mut self,
        registration: WindowRegistration,
        window_id: WindowId,
        cx: &mut Context<Self>,
    ) -> bool {
        let released = self.window_registry.release(registration, window_id);
        if released {
            self.schedule_window_effect_delivery(cx);
        }
        released
    }

    pub(in crate::workspace) fn enqueue_window_intent(
        &mut self,
        intent: &window_intent::WindowIntent,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = intent.take() else {
            return;
        };
        // The action is moved, not cloned, because temporary SSH launches can own a password.
        self.enqueue_window_effect(WorkspaceWindowEffect::WindowIntent(action), cx);
    }

    pub(in crate::workspace) fn enqueue_runtime_window_effect(
        &mut self,
        event: runtime_entity::WorkspaceRuntimeEvent,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_window_effect(WorkspaceWindowEffect::Runtime(event), cx);
    }

    pub(in crate::workspace) fn enqueue_connection_flow_window_effect(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_window_effect(WorkspaceWindowEffect::ConnectionFlow, cx);
    }

    pub(in crate::workspace) fn enqueue_cloud_sync_window_effect(
        &mut self,
        event: cloud_sync::CloudSyncWorkspaceEvent,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_window_effect(WorkspaceWindowEffect::CloudSync(event), cx);
    }

    pub(in crate::workspace) fn enqueue_plugin_window_effect(
        &mut self,
        event: &plugin_entity::PluginWorkspaceEvent,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_window_effect(WorkspaceWindowEffect::Plugin(event.into()), cx);
    }

    pub(in crate::workspace) fn enqueue_tab_host_window_effect(
        &mut self,
        event: tabs::WorkspaceTabHostEvent,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_window_effect(WorkspaceWindowEffect::TabHost(event), cx);
    }

    pub(in crate::workspace) fn enqueue_graphics_window_effect(&mut self, cx: &mut Context<Self>) {
        self.enqueue_window_effect(WorkspaceWindowEffect::Graphics, cx);
    }

    fn enqueue_window_effect(&mut self, effect: WorkspaceWindowEffect, cx: &mut Context<Self>) {
        let coalescing_key = effect.coalescing_key();
        let target_hint = effect.target_hint();
        self.window_registry
            .enqueue(effect, coalescing_key, target_hint);
        self.schedule_window_effect_delivery(cx);
    }

    fn schedule_window_effect_delivery(&mut self, cx: &mut Context<Self>) {
        if self.window_effect_delivery_scheduled
            || self.window_registry.pending_effects.is_empty()
            || self.window_registry.windows.is_empty()
        {
            return;
        }
        self.window_effect_delivery_scheduled = true;
        cx.spawn(async move |workspace, cx| {
            let started_at = Instant::now();
            let mut attempted = 0;
            while delivery::NOTIFICATION_DELIVERY_BUDGET
                .allows_next(attempted, started_at.elapsed())
            {
                let delivery = workspace
                    .update(cx, |workspace, _cx| {
                        workspace.window_registry.next_delivery()
                    })
                    .ok()
                    .flatten();
                let Some(delivery) = delivery else {
                    break;
                };
                attempted += 1;

                // The shared slot lets a failed native-window update return the same
                // owned effect without cloning secret-bearing WindowIntent payloads.
                let effect_slot = Rc::new(RefCell::new(Some(delivery.effect)));
                let window_effect_slot = effect_slot.clone();
                let update_result = cx.update_window(delivery.handle, |_, window, cx| {
                    let Some(effect) = window_effect_slot.borrow_mut().take() else {
                        return;
                    };
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.apply_window_effect(effect, delivery.handle, window, cx);
                    });
                });
                if update_result.is_err() {
                    let Some(effect) = effect_slot.borrow_mut().take() else {
                        continue;
                    };
                    let _ = workspace.update(cx, |workspace, _cx| {
                        workspace
                            .window_registry
                            .release(delivery.registration, delivery.window_id);
                        workspace.window_registry.requeue_front(
                            effect,
                            delivery.coalescing_key,
                            delivery.target_hint,
                        );
                    });
                }
            }
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.window_effect_delivery_scheduled = false;
                if !workspace.window_registry.pending_effects.is_empty()
                    && !workspace.window_registry.windows.is_empty()
                {
                    workspace.schedule_window_effect_delivery(cx);
                }
            });
        })
        .detach();
    }

    fn apply_window_effect(
        &mut self,
        effect: WorkspaceWindowEffect,
        window_handle: AnyWindowHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match effect {
            WorkspaceWindowEffect::WindowIntent(action) => {
                self.handle_window_intent(action, window, cx);
            }
            WorkspaceWindowEffect::Runtime(event) => {
                self.handle_workspace_runtime_event(&event, window, cx);
            }
            WorkspaceWindowEffect::ConnectionFlow => {
                self.apply_connection_flow_worker_delivery(window, cx);
            }
            WorkspaceWindowEffect::CloudSync(event) => {
                self.handle_cloud_sync_workspace_event(&event, window, cx);
            }

            WorkspaceWindowEffect::Plugin(event) => {
                self.handle_plugin_workspace_event(&event.into_event(), window_handle, cx);
            }
            WorkspaceWindowEffect::TabHost(event) => {
                self.handle_tab_host_event(&event, window_handle, cx);
            }
            WorkspaceWindowEffect::Graphics => {
                self.apply_graphics_worker_results(window, cx);
            }
        }
    }
}
