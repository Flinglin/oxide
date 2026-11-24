use oxide_terminal_core::TerminalRelay;
use std::fmt;
mod command_palette;

pub use command_palette::{
    CommandPaletteMatch, CommandPaletteMode, command_palette_match, parse_command_palette_query,
};

pub const MAX_PANES_PER_TAB: usize = 4;
pub const MIN_PANE_FRACTION: f32 = 10.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TerminalSessionId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TabId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PaneId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TabKind {
    Workspace,
    LocalTerminal(TerminalRelay),
    SshTerminal,
    MoshTerminal,
    FileManager,
    Graphics,
    Runtime,
    ConnectionPool,
    Topology,
    NotificationCenter,
    Sftp,
    Ide,
    Forwards,
    SessionManager,
    PluginManager,
    Plugin { plugin_id: String, tab_id: String },
    CloudSync,
    RemoteDesktop,
    Settings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TabTitleSource {
    Static,
    I18nKey(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActiveSessionReadiness {
    Ready,
    Connecting,
    Error,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSessionNode {
    pub id: String,
    pub title: String,
    pub port: u16,
    pub terminal_ids: Vec<TerminalSessionId>,
    pub readiness: ActiveSessionReadiness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveSessionStatus {
    Idle,
    Connecting,
    Connected,
    Active,
    Error,
}

impl ActiveSessionNode {
    pub fn has_terminals(&self) -> bool {
        !self.terminal_ids.is_empty()
    }

    pub fn status(&self) -> ActiveSessionStatus {
        match self.readiness {
            ActiveSessionReadiness::Connecting => ActiveSessionStatus::Connecting,
            ActiveSessionReadiness::Ready if self.has_terminals() => ActiveSessionStatus::Active,
            ActiveSessionReadiness::Ready => ActiveSessionStatus::Connected,
            ActiveSessionReadiness::Error => ActiveSessionStatus::Error,
            ActiveSessionReadiness::Disconnected => ActiveSessionStatus::Idle,
        }
    }
}

pub fn sort_active_session_nodes(nodes: &mut [ActiveSessionNode]) {
    nodes.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub kind: TabKind,
    pub title: String,
    pub title_source: TabTitleSource,
    pub root_pane: Option<PaneNode>,
    pub active_pane_id: Option<PaneId>,
}

/// A split child keeps its layout share beside the node it sizes.
///
/// Keeping these values together makes it impossible for tree edits to leave
/// parallel child and size vectors with different lengths.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneSplitChild {
    pub node: PaneNode,
    pub size: f32,
}

impl PaneSplitChild {
    pub fn new(node: PaneNode, size: f32) -> Self {
        Self { node, size }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneNode {
    Page {
        pane_id: PaneId,
        tab_id: TabId,
    },
    Leaf {
        pane_id: PaneId,
        session_id: TerminalSessionId,
    },
    Group {
        id: PaneId,
        direction: SplitDirection,
        children: Vec<PaneSplitChild>,
    },
}

impl PaneNode {
    pub fn leaf(pane_id: PaneId, session_id: TerminalSessionId) -> Self {
        Self::Leaf {
            pane_id,
            session_id,
        }
    }

    pub fn page_id_for_pane(&self, target: PaneId) -> Option<TabId> {
        match self {
            Self::Page { pane_id, tab_id } if *pane_id == target => Some(*tab_id),
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.page_id_for_pane(target)),
            _ => None,
        }
    }

    pub fn page_pane(&self, target: TabId) -> Option<PaneId> {
        match self {
            Self::Page { pane_id, tab_id } if *tab_id == target => Some(*pane_id),
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.page_pane(target)),
            _ => None,
        }
    }

    pub fn collect_page_ids(&self, pages: &mut Vec<TabId>) {
        match self {
            Self::Page { tab_id, .. } => pages.push(*tab_id),
            Self::Group { children, .. } => {
                for child in children {
                    child.node.collect_page_ids(pages);
                }
            }
            _ => {}
        }
    }

    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf { .. } | Self::Page { .. } => 1,
            Self::Group { children, .. } => {
                children.iter().map(|child| child.node.pane_count()).sum()
            }
        }
    }

    pub fn first_pane_id(&self) -> PaneId {
        match self {
            Self::Leaf { pane_id, .. } | Self::Page { pane_id, .. } => *pane_id,
            Self::Group { children, .. } => children[0].node.first_pane_id(),
        }
    }

    pub fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf { pane_id, .. } | Self::Page { pane_id, .. } => *pane_id == target,
            Self::Group { children, .. } => children
                .iter()
                .any(|child| child.node.contains_pane(target)),
        }
    }

    pub fn pane_id_for_session(&self, target: TerminalSessionId) -> Option<PaneId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *session_id == target => Some(*pane_id),
            Self::Leaf { .. } | Self::Page { .. } => None,
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.pane_id_for_session(target)),
        }
    }

    pub fn session_id_for_pane(&self, target: PaneId) -> Option<TerminalSessionId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *pane_id == target => Some(*session_id),
            Self::Leaf { .. } | Self::Page { .. } => None,
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.session_id_for_pane(target)),
        }
    }

    pub fn replace_session(
        &mut self,
        old_session_id: TerminalSessionId,
        new_pane_id: PaneId,
        new_session_id: TerminalSessionId,
    ) -> Option<PaneId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *session_id == old_session_id => {
                let old_pane_id = *pane_id;
                *pane_id = new_pane_id;
                *session_id = new_session_id;
                Some(old_pane_id)
            }
            Self::Leaf { .. } | Self::Page { .. } => None,
            Self::Group { children, .. } => children.iter_mut().find_map(|child| {
                child
                    .node
                    .replace_session(old_session_id, new_pane_id, new_session_id)
            }),
        }
    }

    pub fn collect_pane_ids(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Leaf { pane_id, .. } | Self::Page { pane_id, .. } => panes.push(*pane_id),
            Self::Group { children, .. } => {
                for child in children {
                    child.node.collect_pane_ids(panes);
                }
            }
        }
    }

    pub fn collect_session_ids(&self, sessions: &mut Vec<TerminalSessionId>) {
        match self {
            Self::Leaf { session_id, .. } => sessions.push(*session_id),
            Self::Page { .. } => {}
            Self::Group { children, .. } => {
                for child in children {
                    child.node.collect_session_ids(sessions);
                }
            }
        }
    }

    pub fn split_active(
        &mut self,
        active_pane_id: PaneId,
        group_id: PaneId,
        direction: SplitDirection,
        new_pane_id: PaneId,
        new_session_id: TerminalSessionId,
    ) -> bool {
        self.split_active_with_node(
            active_pane_id,
            group_id,
            direction,
            Self::leaf(new_pane_id, new_session_id),
        )
    }

    /// Inserts an already-owned pane subtree without recreating its terminal sessions.
    pub fn split_active_with_node(
        &mut self,
        active_pane_id: PaneId,
        group_id: PaneId,
        direction: SplitDirection,
        new_node: PaneNode,
    ) -> bool {
        self.insert_beside(active_pane_id, group_id, direction, new_node, false)
    }

    /// The destination leaf and side are explicit so a drop never depends on current focus.
    pub fn insert_beside(
        &mut self,
        active_pane_id: PaneId,
        group_id: PaneId,
        direction: SplitDirection,
        new_node: PaneNode,
        before: bool,
    ) -> bool {
        match self {
            Self::Leaf { pane_id, .. } | Self::Page { pane_id, .. }
                if *pane_id == active_pane_id =>
            {
                let old = self.clone();
                let nodes = if before {
                    [new_node, old]
                } else {
                    [old, new_node]
                };
                *self = Self::Group {
                    id: group_id,
                    direction,
                    children: nodes
                        .into_iter()
                        .map(|node| PaneSplitChild::new(node, 50.0))
                        .collect(),
                };
                true
            }
            Self::Leaf { .. } | Self::Page { .. } => false,
            Self::Group { children, .. } => {
                let Some(child) = children
                    .iter_mut()
                    .find(|child| child.node.contains_pane(active_pane_id))
                else {
                    return false;
                };
                child
                    .node
                    .insert_beside(active_pane_id, group_id, direction, new_node, before)
            }
        }
    }

    pub fn close_pane(&mut self, target: PaneId) -> Option<PaneId> {
        match self {
            Self::Leaf { .. } | Self::Page { .. } => None,
            Self::Group { children, .. } => {
                let mut removed = false;
                let mut index = 0;
                while index < children.len() {
                    if matches!(&children[index].node, Self::Leaf { pane_id, .. } | Self::Page { pane_id, .. } if *pane_id == target)
                    {
                        children.remove(index);
                        removed = true;
                        break;
                    }
                    if children[index].node.contains_pane(target) {
                        let fallback = children[index].node.close_pane(target);
                        if let Some(replacement) = children[index].node.single_child_replacement() {
                            children[index].node = replacement;
                        }
                        removed = fallback.is_some();
                        break;
                    }
                    index += 1;
                }

                if !removed {
                    return None;
                }

                normalize_children(children);
                children.first().map(|child| child.node.first_pane_id())
            }
        }
    }

    pub fn single_child_replacement(&mut self) -> Option<PaneNode> {
        match self {
            Self::Group { children, .. } if children.len() == 1 => Some(children.remove(0).node),
            _ => None,
        }
    }

    pub fn update_group_sizes(&mut self, group_id: PaneId, next_sizes: &[f32]) -> bool {
        match self {
            Self::Leaf { .. } | Self::Page { .. } => false,
            Self::Group { id, children, .. }
                if *id == group_id && next_sizes.len() == children.len() =>
            {
                let sizes = balanced_sizes(next_sizes, children.len());
                for (child, size) in children.iter_mut().zip(sizes) {
                    child.size = size;
                }
                true
            }
            Self::Group { children, .. } => children
                .iter_mut()
                .any(|child| child.node.update_group_sizes(group_id, next_sizes)),
        }
    }

    pub fn reset_group_sizes(&mut self, group_id: PaneId) -> bool {
        match self {
            Self::Leaf { .. } | Self::Page { .. } => false,
            Self::Group { id, children, .. } if *id == group_id => {
                // Reset only the addressed split group so nested pane ratios
                // remain untouched when a sibling divider is double-clicked.
                let sizes = equal_sizes(children.len());
                for (child, size) in children.iter_mut().zip(sizes) {
                    child.size = size;
                }
                true
            }
            Self::Group { children, .. } => children
                .iter_mut()
                .any(|child| child.node.reset_group_sizes(group_id)),
        }
    }

    pub fn split_sizes(&self) -> Vec<f32> {
        match self {
            Self::Group { children, .. } => balanced_sizes(
                &children.iter().map(|child| child.size).collect::<Vec<_>>(),
                children.len(),
            ),
            Self::Leaf { .. } | Self::Page { .. } => Vec::new(),
        }
    }
}

pub fn adjusted_split_sizes(
    start_sizes: &[f32],
    handle_index: usize,
    delta_fraction: f32,
) -> Vec<f32> {
    if handle_index + 1 >= start_sizes.len() {
        return start_sizes.to_vec();
    }

    let mut sizes = start_sizes.to_vec();
    let left = start_sizes[handle_index] + delta_fraction;
    let total = start_sizes[handle_index] + start_sizes[handle_index + 1];
    let left = left.clamp(MIN_PANE_FRACTION, total - MIN_PANE_FRACTION);
    sizes[handle_index] = left;
    sizes[handle_index + 1] = total - left;
    sizes
}

pub fn balanced_sizes(sizes: &[f32], count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if sizes.len() != count {
        return equal_sizes(count);
    }
    let total: f32 = sizes.iter().copied().sum();
    if total <= f32::EPSILON {
        return equal_sizes(count);
    }
    sizes.iter().map(|size| (size / total) * 100.0).collect()
}

pub fn equal_sizes(count: usize) -> Vec<f32> {
    vec![100.0 / count as f32; count]
}

fn normalize_children(children: &mut [PaneSplitChild]) {
    let sizes = balanced_sizes(
        &children.iter().map(|child| child.size).collect::<Vec<_>>(),
        children.len(),
    );
    for (child, size) in children.iter_mut().zip(sizes) {
        child.size = size;
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tab-{}", self.0)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pane-{}", self.0)
    }
}
