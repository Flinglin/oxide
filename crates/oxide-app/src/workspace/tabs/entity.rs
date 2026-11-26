use super::*;

const MAX_TAB_HISTORY: usize = 50;
const RECORDING_ELAPSED_TICK_INTERVAL: Duration = Duration::from_millis(530);

/// Owns workspace-wide tab identity, terminal mounts, navigation, and close lifecycle.
pub(in crate::workspace) struct WorkspaceTabHostEntity {
    tabs: Vec<Tab>,
    scope: TabScope,
    affinities: HashMap<TabId, Vec<TabAffinity>>,
    scope_selections: HashMap<TabScope, TabId>,
    // Utility descriptors retain their IDs while only their container appears in the tab strip.
    embedded_pages: HashMap<TabId, TabId>,
    active_tab_id: Option<TabId>,
    active_tab_index_cache: Cell<Option<(TabId, usize)>>,
    next_tab_id: u64,
    next_pane_id: u64,
    next_session_id: u64,
    panes: HashMap<PaneId, Entity<TerminalPane>>,
    pane_subscriptions: HashMap<PaneId, Subscription>,
    pane_window_affinities: HashMap<PaneId, TerminalPaneWindowAffinity>,
    tab_mounts: HashMap<TabId, TabMount>,
    pending_detach_mounts: HashMap<TabId, TabMountId>,
    next_tab_mount_id: u64,
    terminal_locations: HashMap<TerminalSessionId, TerminalLocation>,
    pub(in crate::workspace) local_sessions:
        HashMap<TerminalSessionId, super::super::local_sessions::LocalTerminalInstance>,
    terminal_output_highlight_enabled: bool,
    tabs_with_unread_terminal_output: HashSet<TabId>,
    navigation_history: Vec<TabId>,
    navigation_index: Option<usize>,
    navigation_replaying: bool,
    navigation_observed_tab: Option<TabId>,
    process_close_check_generation: u64,
    process_close_check_task: Option<gpui::Task<()>>,
    process_close_completion: Option<TabCloseProcessCompletion>,
    close_confirm: Option<TabCloseConfirm>,
    close_confirm_presence: oxide_ui::motion::ExitPresence,
    close_confirm_focused_action: Option<ConfirmDialogAction>,
    close_confirm_exit_task: Option<gpui::Task<()>>,
    recording_elapsed_pane_id: Option<PaneId>,
    recording_elapsed_generation: u64,
    recording_elapsed_task: Option<gpui::Task<()>>,
}

/// Identifies the single tab and pane currently mounting one terminal session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct TerminalLocation {
    pub(in crate::workspace) tab_id: TabId,
    pub(in crate::workspace) pane_id: PaneId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) struct TabMountId(u64);

/// Identifies the native window currently mounting a workspace tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum TabMount {
    Detached {
        mount_id: TabMountId,
        window_id: gpui::WindowId,
        handle: AnyWindowHandle,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum TabMountCloseReason {
    ReturnToMain,
    TabClosed,
}

/// Describes the native-window cleanup that follows an Entity-owned mount transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct TabMountCleanupPlan {
    pub(in crate::workspace) tab_id: TabId,
    pub(in crate::workspace) reason: TabMountCloseReason,
    pub(in crate::workspace) detached_window: Option<AnyWindowHandle>,
}

pub(in crate::workspace) struct TabRemovalTransition {
    pub(in crate::workspace) tab: Tab,
    pub(in crate::workspace) mount_cleanup: TabMountCleanupPlan,
    pub(in crate::workspace) previous_active_tab_id: Option<TabId>,
    pub(in crate::workspace) next_active_tab_id: Option<TabId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct MainTabSelectionChange {
    pub(in crate::workspace) previous: Option<TabId>,
    pub(in crate::workspace) current: Option<TabId>,
}

pub(in crate::workspace) struct TabDetachTransition {
    pub(in crate::workspace) mount_id: TabMountId,
    pub(in crate::workspace) selection: MainTabSelectionChange,
}

pub(in crate::workspace) struct TabReturnTransition {
    pub(in crate::workspace) cleanup: TabMountCleanupPlan,
    pub(in crate::workspace) selection: MainTabSelectionChange,
}

#[derive(Clone, Copy)]
struct TerminalPaneWindowAffinity {
    home: AnyWindowHandle,
    current: AnyWindowHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum WorkspaceTabHostEvent {
    CloseProcessCheckReady,
    RecordingElapsedTick {
        pane_id: PaneId,
    },
    TerminalOutputUnread,
    TerminalPaneDelivery {
        pane_id: PaneId,
        session_id: TerminalSessionId,
        window_handle: AnyWindowHandle,
        event: TerminalPaneEvent,
    },
}

pub(in crate::workspace) struct TabCloseProcessProbe {
    pub(in crate::workspace) pane_id: PaneId,
    pub(in crate::workspace) probe: Option<oxide_terminal_core::TerminalProcessProbe>,
    pub(in crate::workspace) cached: oxide_terminal_core::TerminalProcessInfo,
}

pub(in crate::workspace) struct TabCloseProcessCompletion {
    pub(in crate::workspace) request: LocalTerminalCloseCheck,
    pub(in crate::workspace) results: Vec<(PaneId, oxide_terminal_core::TerminalProcessInfo)>,
    pub(in crate::workspace) has_foreground_child: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct TabCloseConfirmSnapshot {
    pub(in crate::workspace) confirm: TabCloseConfirm,
    pub(in crate::workspace) phase: oxide_ui::motion::ExitPhase,
    pub(in crate::workspace) focused_action: Option<ConfirmDialogAction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum TabCloseConfirmKeyAction {
    Cancel,
    Confirm,
    Handled,
}

impl WorkspaceTabHostEntity {
    pub(in crate::workspace) fn new() -> Self {
        Self {
            tabs: Vec::new(),
            embedded_pages: HashMap::new(),
            active_tab_id: None,
            scope: TabScope::All,
            affinities: HashMap::new(),
            scope_selections: HashMap::new(),
            active_tab_index_cache: Cell::new(None),
            next_tab_id: 1,
            next_pane_id: 1,
            next_session_id: 1,
            panes: HashMap::new(),
            pane_subscriptions: HashMap::new(),
            pane_window_affinities: HashMap::new(),
            tab_mounts: HashMap::new(),
            pending_detach_mounts: HashMap::new(),
            next_tab_mount_id: 1,
            terminal_locations: HashMap::new(),
            local_sessions: HashMap::new(),
            terminal_output_highlight_enabled: true,
            tabs_with_unread_terminal_output: HashSet::new(),
            navigation_history: Vec::new(),
            navigation_index: None,
            navigation_replaying: false,
            navigation_observed_tab: None,
            process_close_check_generation: 0,
            process_close_check_task: None,
            process_close_completion: None,
            close_confirm: None,
            close_confirm_presence: oxide_ui::motion::ExitPresence::visible(),
            close_confirm_focused_action: None,
            close_confirm_exit_task: None,
            recording_elapsed_pane_id: None,
            recording_elapsed_generation: 0,
            recording_elapsed_task: None,
        }
    }

    pub(in crate::workspace) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub(in crate::workspace) fn active_tab_id(&self) -> Option<TabId> {
        self.active_tab_id
    }

    /// The strip, navigation, and batch actions share this ordered window-local list.
    /// Indices still address the canonical collection used by tab lifecycle operations.
    pub(in crate::workspace) fn main_tabs(&self) -> impl DoubleEndedIterator<Item = (usize, &Tab)> {
        self.unfiltered_main_tabs()
            .filter(|(_, tab)| self.matches_scope(tab.id))
    }

    pub(in crate::workspace) fn unfiltered_main_tabs(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, &Tab)> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| !self.is_outside_main_window(tab.id))
    }
    pub(in crate::workspace) fn scope(&self) -> &TabScope {
        &self.scope
    }

    fn matches_scope(&self, tab: TabId) -> bool {
        match &self.scope {
            TabScope::All => true,
            TabScope::Affinity(affinity) => self
                .affinities
                .get(&tab)
                .is_some_and(|values| values.contains(affinity)),
        }
    }

    pub(in crate::workspace) fn update_affinities(
        &mut self,
        affinities: HashMap<TabId, Vec<TabAffinity>>,
    ) {
        self.affinities = affinities;
        self.scope_selections
            .retain(|_, id| self.tabs.iter().any(|tab| tab.id == *id));
    }

    pub(in crate::workspace) fn prepare_scope(&mut self, scope: TabScope) {
        if let Some(active) = self.active_tab_id.filter(|id| self.matches_scope(*id)) {
            self.scope_selections.insert(self.scope.clone(), active);
        }
        self.scope = scope;
    }

    pub(in crate::workspace) fn switch_scope(&mut self, scope: TabScope) -> Option<TabId> {
        // Clearing a filter reveals other tabs without leaving the current page.
        // Empty scopes still fall back to the remembered selection.
        let keep_active = (scope == TabScope::All)
            .then_some(self.active_tab_id)
            .flatten();
        self.prepare_scope(scope);
        let remembered = self.scope_selections.get(&self.scope).copied();
        let next = keep_active
            .filter(|id| self.main_tabs().any(|(_, tab)| tab.id == *id))
            .or(remembered.filter(|id| self.main_tabs().any(|(_, tab)| tab.id == *id)))
            .or_else(|| self.main_tabs().next().map(|(_, tab)| tab.id));
        self.select_main_tab(next)
    }

    pub(in crate::workspace) fn reconcile_scope_selection(&mut self) -> Option<TabId> {
        if self
            .active_tab_id
            .is_some_and(|id| self.main_tabs().any(|(_, tab)| tab.id == id))
        {
            return self.active_tab_id;
        }
        let next = self.main_tabs().next().map(|(_, tab)| tab.id);
        self.select_main_tab(next)
    }
    pub(in crate::workspace) fn other_main_tab_ids(&self, tab_id: TabId) -> Vec<TabId> {
        self.main_tabs()
            .filter(|(_, tab)| tab.id != tab_id)
            .map(|(_, tab)| tab.id)
            .collect()
    }
    pub(in crate::workspace) fn active_tab_index(&self) -> Option<usize> {
        let active_tab_id = self.active_tab_id?;
        if let Some((cached_tab_id, cached_index)) = self.active_tab_index_cache.get()
            && cached_tab_id == active_tab_id
            && self
                .tabs
                .get(cached_index)
                .is_some_and(|tab| tab.id == active_tab_id)
        {
            return Some(cached_index);
        }
        let index = self.tabs.iter().position(|tab| tab.id == active_tab_id)?;
        self.active_tab_index_cache
            .set(Some((active_tab_id, index)));
        Some(index)
    }

    pub(in crate::workspace) fn tab_index_by_id(&self, tab_id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == tab_id)
    }

    pub(in crate::workspace) fn tab_by_id(&self, tab_id: TabId) -> Option<&Tab> {
        self.tab_index_by_id(tab_id)
            .and_then(|index| self.tabs.get(index))
    }

    /// Renames only terminal display metadata without changing pane or session ownership.
    pub(in crate::workspace) fn rename_terminal_tab(&mut self, tab_id: TabId, title: &str) -> bool {
        let normalized_title = title.trim();
        if normalized_title.is_empty() {
            return false;
        }
        let Some(tab) = self.tab_mut_by_id(tab_id) else {
            return false;
        };
        if !matches!(
            tab.kind,
            TabKind::LocalTerminal(_) | TabKind::SshTerminal | TabKind::MoshTerminal
        ) {
            return false;
        }
        tab.title.clear();
        tab.title.push_str(normalized_title);
        tab.title_source = TabTitleSource::Static;
        true
    }

    fn tab_mut_by_id(&mut self, tab_id: TabId) -> Option<&mut Tab> {
        let index = self.tab_index_by_id(tab_id)?;
        self.tabs.get_mut(index)
    }

    pub(in crate::workspace) fn active_tab(&self) -> Option<&Tab> {
        self.active_tab_index()
            .and_then(|index| self.tabs.get(index))
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let index = self.active_tab_index()?;
        self.tabs.get_mut(index)
    }

    /// Inserts a tab into the canonical collection and invalidates index projections.
    pub(in crate::workspace) fn insert_tab(&mut self, tab: Tab) {
        debug_assert!(
            self.tab_by_id(tab.id).is_none(),
            "tab identity must be unique inside one workspace"
        );
        self.tabs.push(tab);
        self.active_tab_index_cache.set(None);
    }

    pub(in crate::workspace) fn insert_and_select_main_tab(&mut self, tab: Tab) -> Option<TabId> {
        let tab_id = tab.id;
        self.insert_tab(tab);
        self.select_main_tab(Some(tab_id))
    }

    /// Removes one canonical tab and chooses the next main-window selection atomically.
    pub(in crate::workspace) fn remove_tab_at(
        &mut self,
        index: usize,
    ) -> Option<TabRemovalTransition> {
        let page_id = self.tabs.get(index)?.id;
        if let Some(container) = self.embedded_pages.remove(&page_id) {
            if let Some(tab) = self.tab_mut_by_id(container) {
                if tab
                    .root_pane
                    .as_ref()
                    .is_some_and(|root| root.pane_count() == 1)
                {
                    tab.root_pane = None;
                    tab.active_pane_id = None;
                } else if let Some(pane) = tab
                    .root_pane
                    .as_ref()
                    .and_then(|root| root.page_pane(page_id))
                {
                    self.close_pane(container, pane);
                }
            }
        }
        let previous_active_tab_id = self.active_tab_id;
        let tab = self.tabs.get(index)?;
        let removed_was_active = Some(tab.id) == previous_active_tab_id;
        let tab = self.tabs.remove(index);
        self.tabs_with_unread_terminal_output.remove(&tab.id);
        let mount_cleanup = self.close_tab_mount(tab.id);
        let next_active_tab_id = if !removed_was_active
            && previous_active_tab_id.is_some_and(|tab_id| {
                self.tab_by_id(tab_id)
                    .is_some_and(|tab| !self.is_outside_main_window(tab.id))
            }) {
            previous_active_tab_id
        } else {
            self.main_tabs()
                .find(|(i, _)| *i >= index.min(self.tabs.len().saturating_sub(1)))
                .or_else(|| self.main_tabs().rev().find(|(i, _)| *i < index))
                .map(|(_, tab)| tab.id)
        };
        self.select_main_tab(next_active_tab_id);
        Some(TabRemovalTransition {
            tab,
            mount_cleanup,
            previous_active_tab_id,
            next_active_tab_id,
        })
    }

    /// Reorders a main-window tab among the visible main-window tabs.
    pub(in crate::workspace) fn move_main_tab_to_visible_index(
        &mut self,
        tab_id: TabId,
        target_visible_index: usize,
    ) -> bool {
        let Some(source_index) = self
            .main_tabs()
            .find(|(_, tab)| tab.id == tab_id)
            .map(|(i, _)| i)
        else {
            return false;
        };
        let visible_slots = self.main_tabs().map(|(index, _)| index).collect::<Vec<_>>();
        let mut visible_tab_ids = self.main_tabs().map(|(_, tab)| tab.id).collect::<Vec<_>>();
        if self.scope != TabScope::All {
            let source = visible_slots
                .iter()
                .position(|index| *index == source_index)
                .unwrap();
            let target = target_visible_index.min(visible_slots.len().saturating_sub(1));
            if source == target {
                return false;
            }
            let moved = visible_tab_ids.remove(source);
            visible_tab_ids.insert(target, moved);
            // Rotate only visible slots: hidden and detached tabs keep their positions.
            for (slot, id) in visible_slots.into_iter().zip(visible_tab_ids) {
                let index = self.tab_index_by_id(id).unwrap();
                self.tabs.swap(slot, index);
            }
            self.active_tab_index_cache.set(None);
            return true;
        }
        visible_tab_ids.retain(|id| *id != tab_id);
        let target_visible_index = target_visible_index.min(visible_tab_ids.len());
        let anchor_tab_id = visible_tab_ids.get(target_visible_index).copied();
        let trailing_tab_id = target_visible_index
            .checked_sub(1)
            .and_then(|index| visible_tab_ids.get(index).copied());

        let moved_tab = self.tabs.remove(source_index);
        let insertion_index = anchor_tab_id
            .and_then(|anchor_id| self.tab_index_by_id(anchor_id))
            .or_else(|| {
                trailing_tab_id
                    .and_then(|trailing_id| self.tab_index_by_id(trailing_id))
                    .map(|index| index + 1)
            })
            .unwrap_or_else(|| source_index.min(self.tabs.len()))
            .min(self.tabs.len());
        let changed = insertion_index != source_index.min(self.tabs.len());
        self.tabs.insert(insertion_index, moved_tab);
        self.active_tab_index_cache.set(None);
        changed
    }

    pub(in crate::workspace) fn set_active_pane(
        &mut self,
        tab_id: Option<TabId>,
        pane_id: PaneId,
    ) -> bool {
        let tab = match tab_id {
            Some(tab_id) => self.tab_mut_by_id(tab_id),
            None => self.active_tab_mut(),
        };
        let Some(tab) = tab else {
            return false;
        };
        if !tab
            .root_pane
            .as_ref()
            .is_some_and(|root| root.contains_pane(pane_id))
        {
            return false;
        }
        if tab.active_pane_id == Some(pane_id) {
            return false;
        }
        tab.active_pane_id = Some(pane_id);
        true
    }

    pub(in crate::workspace) fn split_pane(
        &mut self,
        tab_id: TabId,
        active_pane_id: PaneId,
        group_id: PaneId,
        direction: SplitDirection,
        pane_id: PaneId,
        session_id: TerminalSessionId,
    ) -> bool {
        let Some(tab) = self.tab_mut_by_id(tab_id) else {
            return false;
        };
        let split = tab.root_pane.as_mut().is_some_and(|root_pane| {
            root_pane.split_active(active_pane_id, group_id, direction, pane_id, session_id)
        });
        if split {
            tab.active_pane_id = Some(pane_id);
        }
        split
    }

    /// Page identities remain stable when their containing layout changes.
    pub(in crate::workspace) fn container_tab_id(&self, page: TabId) -> TabId {
        self.embedded_pages.get(&page).copied().unwrap_or(page)
    }

    pub(in crate::workspace) fn focused_page_id(&self, container: TabId) -> TabId {
        self.tab_by_id(container)
            .and_then(|tab| {
                tab.root_pane
                    .as_ref()?
                    .page_id_for_pane(tab.active_pane_id?)
            })
            .unwrap_or(container)
    }
    pub(in crate::workspace) fn tab_contents(&self, tab_id: TabId) -> Vec<TabContent> {
        fn collect(root: &PaneNode, contents: &mut Vec<TabContent>) {
            match root {
                PaneNode::Page { tab_id, .. } => contents.push(TabContent::Page(*tab_id)),
                PaneNode::Leaf { session_id, .. } => {
                    contents.push(TabContent::Terminal(*session_id))
                }
                PaneNode::Group { children, .. } => {
                    for child in children {
                        collect(&child.node, contents);
                    }
                }
            }
        }
        let Some(tab) = self.tab_by_id(tab_id) else {
            return Vec::new();
        };
        let mut contents = Vec::new();
        if let Some(root) = &tab.root_pane {
            collect(root, &mut contents);
        } else {
            contents.push(TabContent::Page(tab.id));
        }
        contents
    }
    pub(in crate::workspace) fn surface_is_visible(&self, page: TabId) -> bool {
        let container = self.container_tab_id(page);
        self.active_tab_id == Some(container) || self.is_detached(container)
    }

    pub(in crate::workspace) fn unembed_page(&mut self, page: TabId) -> Option<TabId> {
        let container = self.embedded_pages.remove(&page)?;
        let tab = self.tab_mut_by_id(container)?;
        let root = tab.root_pane.as_ref()?;
        let pane = root.page_pane(page)?;
        if root.pane_count() == 1 {
            tab.root_pane = None;
            tab.active_pane_id = None;
        } else {
            self.close_pane(container, pane);
        }
        Some(container)
    }

    pub(in crate::workspace) fn focus_content_page(&mut self, page: TabId) {
        let container = self.container_tab_id(page);
        if let Some(tab) = self.tab_mut_by_id(container) {
            if let Some(pane) = tab.root_pane.as_ref().and_then(|root| root.page_pane(page)) {
                tab.active_pane_id = Some(pane);
            }
        }
    }

    pub(in crate::workspace) fn can_combine_pages(&self, source: TabId, target: TabId) -> bool {
        if source == target
            || self.is_outside_main_window(source)
            || self.is_outside_main_window(target)
        {
            return false;
        }
        self.pages_fit_together(source, target)
    }

    pub(in crate::workspace) fn can_receive_tab_drop(&self, source: TabId, target: TabId) -> bool {
        source != target
            && self.container_tab_id(source) == source
            && !self.pending_detach_mounts.contains_key(&source)
            && !self.is_outside_main_window(target)
            && self.active_tab_id == Some(target)
            && self.pages_fit_together(source, target)
    }

    fn pages_fit_together(&self, source: TabId, target: TabId) -> bool {
        self.tab_by_id(source)
            .zip(self.tab_by_id(target))
            .is_some_and(|(a, b)| {
                a.root_pane.as_ref().map_or(1, PaneNode::pane_count)
                    + b.root_pane.as_ref().map_or(1, PaneNode::pane_count)
                    <= MAX_PANES_PER_TAB
            })
    }

    /// Page owners keep their IDs; only layout and terminal mount indexes change.
    pub(in crate::workspace) fn combine_pages(
        &mut self,
        source: TabId,
        target: TabId,
        direction: SplitDirection,
    ) -> Option<(Tab, Vec<TabId>)> {
        self.combine_pages_at(source, target, None, direction, false)
    }

    pub(in crate::workspace) fn combine_pages_at(
        &mut self,
        source: TabId,
        target: TabId,
        target_pane: Option<PaneId>,
        direction: SplitDirection,
        before: bool,
    ) -> Option<(Tab, Vec<TabId>)> {
        if !self.can_combine_pages(source, target) {
            return None;
        }
        let source_tab = self.tab_by_id(source)?.clone();
        let target_tab = self.tab_by_id(target)?.clone();
        if target_pane.is_some_and(|pane| {
            target_tab
                .root_pane
                .as_ref()
                .is_none_or(|root| !root.contains_pane(pane))
        }) {
            return None;
        }
        let source_root = source_tab
            .root_pane
            .clone()
            .unwrap_or_else(|| PaneNode::Page {
                pane_id: self.alloc_pane_id(),
                tab_id: source,
            });
        let mut root = target_tab
            .root_pane
            .clone()
            .unwrap_or_else(|| PaneNode::Page {
                pane_id: self.alloc_pane_id(),
                tab_id: target,
            });
        let focus = source_tab
            .active_pane_id
            .unwrap_or_else(|| source_root.first_pane_id());
        let target_pane = target_pane
            .or(target_tab.active_pane_id)
            .unwrap_or_else(|| root.first_pane_id());
        let group = self.alloc_pane_id();
        if !root.insert_beside(target_pane, group, direction, source_root, before) {
            return None;
        }
        let id = self.alloc_tab_id();
        let mut pages = Vec::new();
        root.collect_page_ids(&mut pages);
        for page in &pages {
            self.embedded_pages.insert(*page, id);
        }
        let removed = [source, target]
            .into_iter()
            .filter(|id| !pages.contains(id))
            .collect::<Vec<_>>();
        self.tabs.retain(|tab| !removed.contains(&tab.id));
        for old in &removed {
            self.tabs_with_unread_terminal_output.remove(old);
        }
        let mut sessions = Vec::new();
        root.collect_session_ids(&mut sessions);
        for session in sessions {
            if let Some(location) = self.terminal_locations.get_mut(&session) {
                location.tab_id = id;
            }
        }
        let tab = Tab {
            id,
            kind: TabKind::Workspace,
            title: target_tab.title,
            title_source: TabTitleSource::Static,
            root_pane: Some(root),
            active_pane_id: Some(focus),
        };
        self.insert_and_select_main_tab(tab.clone());
        Some((tab, removed))
    }

    /// Moving a pane preserves its Entity, session, subscriptions, and runtime consumers.
    pub(in crate::workspace) fn move_terminal_pane_to_tab(
        &mut self,
        source_id: TabId,
        pane_id: PaneId,
        new_tab: Tab,
        main_window: AnyWindowHandle,
    ) -> bool {
        if self.tab_by_id(new_tab.id).is_some()
            || self.pending_detach_mounts.contains_key(&source_id)
        {
            return false;
        }
        let Some(source) = self.tab_by_id(source_id) else {
            return false;
        };
        let Some(root) = source.root_pane.as_ref() else {
            return false;
        };
        let Some(session_id) = root.session_id_for_pane(pane_id) else {
            return false;
        };
        if root.pane_count() <= 1
            || new_tab.root_pane.as_ref() != Some(&PaneNode::leaf(pane_id, session_id))
        {
            return false;
        }
        if self.close_pane(source_id, pane_id).is_none() {
            return false;
        }
        if let Some(location) = self.terminal_locations.get_mut(&session_id) {
            location.tab_id = new_tab.id;
        }
        // A pane moved from a detached window is mounted in the main window until detached again.
        if let Some(affinity) = self.pane_window_affinities.get_mut(&pane_id) {
            affinity.home = main_window;
            affinity.current = main_window;
        }
        self.insert_tab(new_tab);
        true
    }
    pub(in crate::workspace) fn terminal_pane_for_target(
        &self,
        pane_id: PaneId,
        session_id: TerminalSessionId,
    ) -> Option<Entity<TerminalPane>> {
        let location = self.terminal_location(session_id)?;
        (location.pane_id == pane_id)
            .then(|| self.panes.get(&pane_id).cloned())
            .flatten()
    }
    pub(in crate::workspace) fn close_pane(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
    ) -> Option<PaneId> {
        let tab = self.tab_mut_by_id(tab_id)?;
        let previous_active = tab.active_pane_id;
        let root_pane = tab.root_pane.as_mut()?;
        let next_active_pane_id = root_pane.close_pane(pane_id)?;
        if let Some(replacement) = root_pane.single_child_replacement() {
            tab.root_pane = Some(replacement);
        }
        tab.active_pane_id = previous_active
            .filter(|id| *id != pane_id)
            .or(Some(next_active_pane_id));
        let remaining_page = match &tab.root_pane {
            Some(PaneNode::Page { tab_id, .. }) => Some(*tab_id),
            _ => None,
        };
        if let Some((title, title_source)) = remaining_page
            .and_then(|id| self.tab_by_id(id))
            .map(|page| (page.title.clone(), page.title_source.clone()))
        {
            let tab = self.tab_mut_by_id(tab_id)?;
            tab.title = title;
            tab.title_source = title_source;
        }
        Some(next_active_pane_id)
    }

    pub(in crate::workspace) fn update_group_sizes(
        &mut self,
        tab_id: Option<TabId>,
        group_id: PaneId,
        sizes: &[f32],
    ) -> bool {
        let tab = match tab_id {
            Some(tab_id) => self.tab_mut_by_id(tab_id),
            None => self.active_tab_mut(),
        };
        tab.and_then(|tab| tab.root_pane.as_mut())
            .is_some_and(|root_pane| root_pane.update_group_sizes(group_id, sizes))
    }

    pub(in crate::workspace) fn reset_group_sizes(
        &mut self,
        tab_id: Option<TabId>,
        group_id: PaneId,
    ) -> bool {
        let tab = match tab_id {
            Some(tab_id) => self.tab_mut_by_id(tab_id),
            None => self.active_tab_mut(),
        };
        tab.and_then(|tab| tab.root_pane.as_mut())
            .is_some_and(|root_pane| root_pane.reset_group_sizes(group_id))
    }

    pub(in crate::workspace) fn sync_tab_titles(
        &mut self,
        mut title_for_key: impl FnMut(&'static str) -> String,
    ) {
        for tab in &mut self.tabs {
            if let TabTitleSource::I18nKey(key) = tab.title_source {
                tab.title = title_for_key(key);
            }
        }
    }

    /// Updates the canonical main-window selection and navigation history together.
    pub(in crate::workspace) fn select_main_tab(
        &mut self,
        active_tab_id: Option<TabId>,
    ) -> Option<TabId> {
        let active_tab_id = active_tab_id.map(|page| {
            let container = self.container_tab_id(page);
            if container != page
                && let Some(tab) = self.tab_mut_by_id(container)
            {
                if let Some(pane) = tab.root_pane.as_ref().and_then(|root| root.page_pane(page)) {
                    tab.active_pane_id = Some(pane);
                }
            }
            container
        });
        debug_assert!(
            active_tab_id.is_none_or(|tab_id| self.tab_by_id(tab_id).is_some()),
            "main-window selection must reference a canonical tab"
        );
        let previous_active_tab_id = self.active_tab_id;
        self.active_tab_id = active_tab_id;
        if let Some(tab_id) = active_tab_id {
            self.tabs_with_unread_terminal_output.remove(&tab_id);
        }
        self.active_tab_index_cache.set(None);
        self.observe_active_tab(active_tab_id);
        if let Some(id) = active_tab_id.filter(|id| self.matches_scope(*id)) {
            self.scope_selections.insert(self.scope.clone(), id);
        }
        previous_active_tab_id
    }

    pub(in crate::workspace) fn alloc_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        id
    }

    pub(in crate::workspace) fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        id
    }

    pub(in crate::workspace) fn alloc_session_id(&mut self) -> TerminalSessionId {
        let id = TerminalSessionId(self.next_session_id);
        self.next_session_id += 1;
        id
    }

    pub(in crate::workspace) fn sync_recording_elapsed_tick(
        &mut self,
        pane_id: Option<PaneId>,
        recording: bool,
        cx: &mut Context<Self>,
    ) {
        let pane_id = pane_id.filter(|_| recording);
        if self.recording_elapsed_pane_id == pane_id
            && (pane_id.is_none() || self.recording_elapsed_task.is_some())
        {
            return;
        }
        self.recording_elapsed_pane_id = pane_id;
        self.recording_elapsed_generation = self.recording_elapsed_generation.wrapping_add(1);
        self.recording_elapsed_task = None;
        let Some(pane_id) = pane_id else {
            return;
        };
        let generation = self.recording_elapsed_generation;
        self.recording_elapsed_task = Some(cx.spawn(async move |tab_host, cx| {
            loop {
                Timer::after(RECORDING_ELAPSED_TICK_INTERVAL).await;
                let should_continue = tab_host
                    .update(cx, |tab_host, cx| {
                        if tab_host.recording_elapsed_generation != generation
                            || tab_host.recording_elapsed_pane_id != Some(pane_id)
                        {
                            return false;
                        }
                        cx.emit(WorkspaceTabHostEvent::RecordingElapsedTick { pane_id });
                        true
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        }));
    }

    pub(in crate::workspace) fn bind_terminal_location(
        &mut self,
        session_id: TerminalSessionId,
        location: TerminalLocation,
    ) {
        // A session may be registered repeatedly at the same mount boundary,
        // but moving it requires the previous pane lifecycle to unbind first.
        let previous = self.terminal_locations.insert(session_id, location);
        debug_assert!(
            previous.is_none_or(|previous| previous == location),
            "terminal session was rebound without removing its previous location"
        );
        if let Some(window_handle) = self.detached_window_handle(location.tab_id)
            && let Some(affinity) = self.pane_window_affinities.get_mut(&location.pane_id)
        {
            // Reconnect remounts can register a replacement pane after the tab
            // moved, so location binding reapplies the current window owner.
            affinity.current = window_handle;
        }
    }

    pub(in crate::workspace) fn register_terminal_pane(
        &mut self,
        pane_id: PaneId,
        session_id: TerminalSessionId,
        pane: Entity<TerminalPane>,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        // TabHost owns pane delivery and its cancellation together with the
        // registered Entity. Delivery resolves the current mount dynamically
        // so detach and return cannot retain the creation window.
        self.pane_window_affinities.insert(
            pane_id,
            TerminalPaneWindowAffinity {
                home: window_handle,
                current: window_handle,
            },
        );
        let subscription = cx.subscribe(&pane, move |tab_host, _pane, event, cx| {
            if *event == TerminalPaneEvent::OutputActivity
                && tab_host.mark_terminal_output_unread(session_id).is_some()
            {
                cx.emit(WorkspaceTabHostEvent::TerminalOutputUnread);
            }
            let Some(window_handle) = tab_host
                .pane_window_affinities
                .get(&pane_id)
                .map(|affinity| affinity.current)
            else {
                return;
            };
            cx.emit(WorkspaceTabHostEvent::TerminalPaneDelivery {
                pane_id,
                session_id,
                window_handle,
                event: *event,
            });
        });
        self.pane_subscriptions.insert(pane_id, subscription);
        self.panes.insert(pane_id, pane);
    }

    pub(in crate::workspace) fn remove_terminal_pane(
        &mut self,
        pane_id: PaneId,
    ) -> Option<Entity<TerminalPane>> {
        self.pane_subscriptions.remove(&pane_id);
        self.pane_window_affinities.remove(&pane_id);
        if let Some(session_id) = self.unbind_terminal_location_for_pane(pane_id) {
            self.local_sessions.remove(&session_id);
        }
        self.panes.remove(&pane_id)
    }

    pub(in crate::workspace) fn panes(&self) -> &HashMap<PaneId, Entity<TerminalPane>> {
        &self.panes
    }

    pub(in crate::workspace) fn unbind_terminal_location_for_pane(
        &mut self,
        pane_id: PaneId,
    ) -> Option<TerminalSessionId> {
        let session_id = self
            .terminal_locations
            .iter()
            .find_map(|(session_id, location)| {
                (location.pane_id == pane_id).then_some(*session_id)
            })?;
        self.terminal_locations.remove(&session_id);
        Some(session_id)
    }

    pub(in crate::workspace) fn terminal_location(
        &self,
        session_id: TerminalSessionId,
    ) -> Option<TerminalLocation> {
        self.terminal_locations.get(&session_id).copied()
    }

    pub(in crate::workspace) fn unread_terminal_output_tab_ids(&self) -> HashSet<TabId> {
        // Rendering needs stable identities after releasing the Entity borrow.
        self.tabs_with_unread_terminal_output.clone()
    }

    pub(in crate::workspace) fn configure_terminal_output_highlight(&mut self, enabled: bool) {
        self.terminal_output_highlight_enabled = enabled;
        if !enabled {
            self.tabs_with_unread_terminal_output.clear();
        }
    }

    fn mark_terminal_output_unread(&mut self, session_id: TerminalSessionId) -> Option<TabId> {
        if !self.terminal_output_highlight_enabled {
            return None;
        }
        let tab_id = self.terminal_location(session_id)?.tab_id;
        if self.active_tab_id == Some(tab_id)
            || self.is_outside_main_window(tab_id)
            || self.tab_by_id(tab_id).is_none()
        {
            return None;
        }
        self.tabs_with_unread_terminal_output
            .insert(tab_id)
            .then_some(tab_id)
    }

    pub(in crate::workspace) fn begin_detach(&mut self, tab_id: TabId) -> Option<TabMountId> {
        if self.pending_detach_mounts.contains_key(&tab_id)
            || matches!(
                self.tab_mounts.get(&tab_id),
                Some(TabMount::Detached { .. })
            )
        {
            return None;
        }
        let mount_id = TabMountId(self.next_tab_mount_id);
        self.next_tab_mount_id = self.next_tab_mount_id.wrapping_add(1);
        self.pending_detach_mounts.insert(tab_id, mount_id);
        Some(mount_id)
    }

    pub(in crate::workspace) fn begin_detach_from_main(
        &mut self,
        tab_id: TabId,
    ) -> Option<TabDetachTransition> {
        let tab_index = self.tab_index_by_id(tab_id)?;
        let previous = self.active_tab_id;
        let mount_id = self.begin_detach(tab_id)?;
        self.tabs_with_unread_terminal_output.remove(&tab_id);
        let current = if previous == Some(tab_id) {
            self.nearest_main_tab(tab_index)
        } else {
            previous
        };
        self.select_main_tab(current);
        Some(TabDetachTransition {
            mount_id,
            selection: MainTabSelectionChange { previous, current },
        })
    }

    pub(in crate::workspace) fn commit_detach(
        &mut self,
        tab_id: TabId,
        mount_id: TabMountId,
        window_handle: AnyWindowHandle,
    ) -> bool {
        if self.pending_detach_mounts.get(&tab_id).copied() != Some(mount_id)
            || matches!(
                self.tab_mounts.get(&tab_id),
                Some(TabMount::Detached { .. })
            )
        {
            return false;
        }
        self.pending_detach_mounts.remove(&tab_id);
        self.tab_mounts.insert(
            tab_id,
            TabMount::Detached {
                mount_id,
                window_id: window_handle.window_id(),
                handle: window_handle,
            },
        );
        for location in self
            .terminal_locations
            .values()
            .filter(|location| location.tab_id == tab_id)
        {
            if let Some(affinity) = self.pane_window_affinities.get_mut(&location.pane_id) {
                affinity.current = window_handle;
            }
        }
        true
    }

    pub(in crate::workspace) fn rollback_detach(
        &mut self,
        tab_id: TabId,
        mount_id: TabMountId,
    ) -> bool {
        if self.pending_detach_mounts.get(&tab_id).copied() != Some(mount_id) {
            return false;
        }
        self.pending_detach_mounts.remove(&tab_id);
        true
    }

    pub(in crate::workspace) fn rollback_detach_to_main(
        &mut self,
        tab_id: TabId,
        mount_id: TabMountId,
    ) -> Option<MainTabSelectionChange> {
        if !self.rollback_detach(tab_id, mount_id) {
            return None;
        }
        let previous = self.active_tab_id;
        let current = self.tab_by_id(tab_id).map(|tab| tab.id);
        self.select_main_tab(current);
        Some(MainTabSelectionChange { previous, current })
    }

    pub(in crate::workspace) fn return_to_main(
        &mut self,
        tab_id: TabId,
        reason: TabMountCloseReason,
    ) -> Option<TabMountCleanupPlan> {
        let Some(TabMount::Detached { handle, .. }) = self.tab_mounts.get(&tab_id).copied() else {
            return None;
        };
        self.tab_mounts.remove(&tab_id);
        self.restore_tab_panes_to_home_window(tab_id);
        Some(TabMountCleanupPlan {
            tab_id,
            reason,
            detached_window: Some(handle),
        })
    }

    pub(in crate::workspace) fn return_to_main_and_select(
        &mut self,
        tab_id: TabId,
        reason: TabMountCloseReason,
    ) -> Option<TabReturnTransition> {
        let cleanup = self.return_to_main(tab_id, reason)?;
        let previous = self.active_tab_id;
        let current = self.tab_by_id(tab_id).map(|tab| tab.id);
        self.select_main_tab(current);
        Some(TabReturnTransition {
            cleanup,
            selection: MainTabSelectionChange { previous, current },
        })
    }

    pub(in crate::workspace) fn remove_tab_for_detached_window_release(
        &mut self,
        tab_id: TabId,
        mount_id: TabMountId,
        window_id: gpui::WindowId,
    ) -> Option<TabRemovalTransition> {
        let tab_index = match self.tab_mounts.get(&tab_id).copied() {
            Some(TabMount::Detached {
                mount_id: current_mount_id,
                window_id: current_window_id,
                ..
            }) if current_mount_id == mount_id && current_window_id == window_id => {
                self.tab_index_by_id(tab_id)?
            }
            _ => return None,
        };
        let mut transition = self.remove_tab_at(tab_index)?;
        // The native window is already releasing, so final tab cleanup must not
        // re-enter its handle. Pane and terminal cleanup still follows normally.
        transition.mount_cleanup.detached_window = None;
        Some(transition)
    }

    pub(in crate::workspace) fn close_tab_mount(&mut self, tab_id: TabId) -> TabMountCleanupPlan {
        self.pending_detach_mounts.remove(&tab_id);
        let detached_window = match self.tab_mounts.remove(&tab_id) {
            Some(TabMount::Detached { handle, .. }) => Some(handle),
            _ => None,
        };
        // Pane removal follows in the caller's tab-close transaction. Do not
        // release terminal or node consumers merely because the mount changed.
        TabMountCleanupPlan {
            tab_id,
            reason: TabMountCloseReason::TabClosed,
            detached_window,
        }
    }

    fn restore_tab_panes_to_home_window(&mut self, tab_id: TabId) {
        for location in self
            .terminal_locations
            .values()
            .filter(|location| location.tab_id == tab_id)
        {
            if let Some(affinity) = self.pane_window_affinities.get_mut(&location.pane_id) {
                affinity.current = affinity.home;
            }
        }
    }

    pub(in crate::workspace) fn is_outside_main_window(&self, tab_id: TabId) -> bool {
        self.embedded_pages.contains_key(&tab_id)
            || self.pending_detach_mounts.contains_key(&tab_id)
            || matches!(
                self.tab_mounts.get(&tab_id),
                Some(TabMount::Detached { .. })
            )
    }

    fn nearest_main_tab(&self, index: usize) -> Option<TabId> {
        self.main_tabs()
            .find(|(i, _)| *i > index)
            .or_else(|| self.main_tabs().rev().find(|(i, _)| *i < index))
            .map(|(_, tab)| tab.id)
    }

    pub(in crate::workspace) fn is_detached(&self, tab_id: TabId) -> bool {
        let tab_id = self.container_tab_id(tab_id);
        matches!(
            self.tab_mounts.get(&tab_id),
            Some(TabMount::Detached { .. })
        )
    }

    pub(in crate::workspace) fn detached_window_handle(
        &self,
        tab_id: TabId,
    ) -> Option<AnyWindowHandle> {
        let tab_id = self.container_tab_id(tab_id);
        match self.tab_mounts.get(&tab_id).copied() {
            Some(TabMount::Detached { handle, .. }) => Some(handle),
            _ => None,
        }
    }

    pub(in crate::workspace) fn observe_active_tab(&mut self, active_tab_id: Option<TabId>) {
        if self.navigation_observed_tab == active_tab_id {
            return;
        }
        self.navigation_observed_tab = active_tab_id;

        let Some(tab_id) = active_tab_id else {
            return;
        };
        if self.navigation_replaying {
            self.navigation_replaying = false;
            return;
        }

        if let Some(index) = self.navigation_index {
            self.navigation_history.truncate(index.saturating_add(1));
        }
        if self.navigation_history.last().copied() != Some(tab_id) {
            self.navigation_history.push(tab_id);
        }
        if self.navigation_history.len() > MAX_TAB_HISTORY {
            let overflow = self.navigation_history.len() - MAX_TAB_HISTORY;
            self.navigation_history.drain(0..overflow);
        }
        self.navigation_index = self.navigation_history.len().checked_sub(1);
    }

    pub(in crate::workspace) fn navigate_history(
        &mut self,
        forward: bool,
        existing_tab_ids: &HashSet<TabId>,
    ) -> Option<TabId> {
        self.prune_navigation_history(existing_tab_ids);
        let mut index = self.navigation_index?;

        loop {
            if forward {
                if index + 1 >= self.navigation_history.len() {
                    return None;
                }
                index += 1;
            } else if index == 0 {
                return None;
            } else {
                index -= 1;
            }

            let tab_id = self.navigation_history[index];
            if existing_tab_ids.contains(&tab_id) {
                self.navigation_index = Some(index);
                self.navigation_replaying = true;
                return Some(tab_id);
            }
        }
    }

    fn prune_navigation_history(&mut self, existing_tab_ids: &HashSet<TabId>) {
        let current = self
            .navigation_index
            .and_then(|index| self.navigation_history.get(index).copied());
        self.navigation_history
            .retain(|tab_id| existing_tab_ids.contains(tab_id));
        self.navigation_index = current
            .and_then(|tab_id| {
                self.navigation_history
                    .iter()
                    .position(|candidate| *candidate == tab_id)
            })
            .or_else(|| self.navigation_history.len().checked_sub(1));
    }

    pub(in crate::workspace) fn start_close_process_check(
        &mut self,
        request: LocalTerminalCloseCheck,
        probes: Vec<TabCloseProcessProbe>,
        cx: &mut Context<Self>,
    ) {
        let probe_task = cx.background_executor().spawn(async move {
            // Each probe owns its duplicated PTY descriptor, so no terminal mutex is held while
            // platform process and cwd commands run on the background executor.
            probes
                .into_iter()
                .map(|probe| {
                    let info = probe
                        .probe
                        .map(|probe| probe.collect_foreground_only())
                        .unwrap_or(probe.cached);
                    (probe.pane_id, info)
                })
                .collect::<Vec<_>>()
        });

        self.start_close_process_check_with_future(request, probe_task, cx);
    }

    fn start_close_process_check_with_future(
        &mut self,
        request: LocalTerminalCloseCheck,
        probe_task: impl std::future::Future<
            Output = Vec<(PaneId, oxide_terminal_core::TerminalProcessInfo)>,
        > + 'static,
        cx: &mut Context<Self>,
    ) {
        self.process_close_check_generation = self.process_close_check_generation.wrapping_add(1);
        // A newer user request invalidates a completion that has not reached the window adapter.
        self.process_close_completion = None;
        let generation = self.process_close_check_generation;
        // The Entity is the sole task owner. Replacing this handle cancels the
        // older check, and releasing the Entity cancels the current check.
        self.process_close_check_task = Some(cx.spawn(async move |entity, cx| {
            let results = probe_task.await;
            let _ = entity.update(cx, |entity, cx| {
                if entity.process_close_check_generation != generation {
                    return;
                }
                let has_foreground_child = results
                    .iter()
                    .any(|(_, info)| terminal_process_info_has_foreground_child_process(info));
                entity.process_close_completion = Some(TabCloseProcessCompletion {
                    request,
                    results,
                    has_foreground_child,
                });
                cx.emit(WorkspaceTabHostEvent::CloseProcessCheckReady);
            });
        }));
    }

    pub(in crate::workspace) fn take_close_process_completion(
        &mut self,
    ) -> Option<TabCloseProcessCompletion> {
        self.process_close_completion.take()
    }

    pub(in crate::workspace) fn open_close_confirm(
        &mut self,
        confirm: TabCloseConfirm,
        cx: &mut Context<Self>,
    ) {
        // A replacement confirmation owns a fresh generation and cancels the
        // retained exit task for the previous payload.
        self.close_confirm_exit_task = None;
        self.close_confirm = Some(confirm);
        self.close_confirm_presence.reopen();
        self.close_confirm_focused_action = None;
        cx.notify();
    }

    pub(in crate::workspace) fn close_confirm(&self) -> Option<&TabCloseConfirm> {
        self.close_confirm.as_ref()
    }

    pub(in crate::workspace) fn close_confirm_phase(&self) -> Option<oxide_ui::motion::ExitPhase> {
        self.close_confirm
            .as_ref()
            .map(|_| self.close_confirm_presence.phase())
    }

    pub(in crate::workspace) fn close_confirm_snapshot(&self) -> Option<TabCloseConfirmSnapshot> {
        self.close_confirm
            .as_ref()
            .cloned()
            .map(|confirm| TabCloseConfirmSnapshot {
                confirm,
                phase: self.close_confirm_presence.phase(),
                focused_action: self.close_confirm_focused_action,
            })
    }

    pub(in crate::workspace) fn handle_close_confirm_key(
        &mut self,
        key: &str,
        shift: bool,
        blocked_by_primary_modifier: bool,
        cx: &mut Context<Self>,
    ) -> Option<TabCloseConfirmKeyAction> {
        if blocked_by_primary_modifier
            || self.close_confirm.is_none()
            || self.close_confirm_presence.phase() != oxide_ui::motion::ExitPhase::Visible
        {
            return None;
        }
        if key == "escape" {
            self.close_confirm_focused_action = None;
            return Some(TabCloseConfirmKeyAction::Cancel);
        }
        if key == "enter" {
            self.close_confirm_focused_action = None;
            return Some(TabCloseConfirmKeyAction::Confirm);
        }
        const ACTIONS: [ConfirmDialogAction; 2] =
            [ConfirmDialogAction::Cancel, ConfirmDialogAction::Confirm];
        match browser_behavior::modal_footer_key_action(
            key,
            shift,
            &ACTIONS,
            self.close_confirm_focused_action,
            ConfirmDialogAction::Cancel,
        ) {
            Some(browser_behavior::ModalFooterKeyAction::Cancel) => {
                self.close_confirm_focused_action = None;
                Some(TabCloseConfirmKeyAction::Cancel)
            }
            Some(browser_behavior::ModalFooterKeyAction::Focus(action)) => {
                self.close_confirm_focused_action = Some(action);
                cx.notify();
                Some(TabCloseConfirmKeyAction::Handled)
            }
            Some(browser_behavior::ModalFooterKeyAction::Activate(action)) => {
                self.close_confirm_focused_action = None;
                Some(match action {
                    ConfirmDialogAction::Cancel => TabCloseConfirmKeyAction::Cancel,
                    ConfirmDialogAction::Confirm => TabCloseConfirmKeyAction::Confirm,
                })
            }
            None => None,
        }
    }

    pub(in crate::workspace) fn begin_close_confirm_exit(
        &mut self,
        confirmed: bool,
        delay: Duration,
        cx: &mut Context<Self>,
    ) -> (bool, Option<TabCloseConfirm>) {
        let Some(confirm) = self.close_confirm.as_ref() else {
            return (false, None);
        };
        let Some(generation) = self.close_confirm_presence.begin_exit() else {
            return (false, None);
        };
        self.close_confirm_focused_action = None;
        let effect = confirmed.then(|| confirm.clone());
        self.close_confirm_exit_task = None;
        if delay.is_zero() {
            self.finish_close_confirm_exit(generation, cx);
            return (true, effect);
        }
        self.close_confirm_exit_task = Some(cx.spawn(async move |tab_host, cx| {
            Timer::after(delay).await;
            let _ = tab_host.update(cx, |tab_host, cx| {
                tab_host.finish_close_confirm_exit(generation, cx);
            });
        }));
        cx.notify();
        (true, effect)
    }

    fn finish_close_confirm_exit(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.close_confirm_exit_task = None;
        if self.close_confirm.is_some() && self.close_confirm_presence.finish_exit(generation) {
            self.close_confirm = None;
            self.close_confirm_presence.reopen();
            self.close_confirm_focused_action = None;
            cx.notify();
        }
    }
}

fn terminal_process_info_has_foreground_child_process(
    info: &oxide_terminal_core::TerminalProcessInfo,
) -> bool {
    let Some(shell_pid) = info.shell_pid else {
        return false;
    };
    info.foreground_process_group_id
        .is_some_and(|foreground_group| foreground_group != shell_pid)
        || info
            .foreground_pid
            .is_some_and(|foreground_pid| foreground_pid != shell_pid)
}

impl gpui::EventEmitter<WorkspaceTabHostEvent> for WorkspaceTabHostEntity {}
