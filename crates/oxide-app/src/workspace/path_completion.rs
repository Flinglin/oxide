use super::*;

const PATH_COMPLETION_VISIBLE_ROWS: usize = 8;
const PATH_COMPLETION_MAX_MATCHES: usize = 512;
const PATH_COMPLETION_ROW_HEIGHT: f32 = 28.0;
const PATH_COMPLETION_POPUP_GAP: f32 = 4.0;
const PATH_COMPLETION_BG_ALPHA: u32 = 0xf2;
const PATH_COMPLETION_HOVER_ALPHA: u32 = 0x99;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PathCompletionOwner {
    FileManager,
    SftpLocal(sftp::SftpSurfaceId),
    SftpRemote(sftp::SftpSurfaceId),
}

impl PathCompletionOwner {
    fn ime_target(self) -> WorkspaceImeTarget {
        match self {
            Self::FileManager => {
                WorkspaceImeTarget::FileManager(file_manager::FileManagerInput::Path)
            }
            Self::SftpLocal(surface) => {
                WorkspaceImeTarget::Sftp(surface, sftp::SftpInput::LocalPath)
            }
            Self::SftpRemote(surface) => {
                WorkspaceImeTarget::Sftp(surface, sftp::SftpInput::RemotePath)
            }
        }
    }

    fn popup_id(self) -> &'static str {
        match self {
            Self::FileManager => "file-manager-path-completion",
            Self::SftpLocal(_) => "sftp-local-path-completion",
            Self::SftpRemote(_) => "sftp-remote-path-completion",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PathCompletionRequest {
    pub(super) parent_path: String,
    prefix: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PathCompletionCandidate {
    pub(super) name: String,
    pub(super) path: String,
    pub(super) is_directory: bool,
}

#[derive(Default)]
pub(super) struct PathCompletionState {
    request: Option<PathCompletionRequest>,
    loaded_parent: Option<String>,
    loading_parent: Option<String>,
    entries: Vec<PathCompletionCandidate>,
    suggestions: Vec<PathCompletionCandidate>,
    selected_index: Option<usize>,
    generation: u64,
    scroll_handle: ScrollHandle,
}

impl PathCompletionState {
    /// Updates the query and returns one directory load when cached entries cannot satisfy it.
    pub(super) fn request(&mut self, request: PathCompletionRequest) -> Option<(u64, String)> {
        let parent_path = request.parent_path.clone();
        self.request = Some(request);
        self.selected_index = None;

        if self.loaded_parent.as_deref() == Some(parent_path.as_str()) {
            self.rebuild_suggestions();
            return None;
        }
        self.suggestions.clear();
        if self.loading_parent.as_deref() == Some(parent_path.as_str()) {
            return None;
        }

        self.generation = self.generation.wrapping_add(1);
        self.loading_parent = Some(parent_path.clone());
        self.loaded_parent = None;
        self.entries.clear();
        Some((self.generation, parent_path))
    }

    pub(super) fn apply_entries(
        &mut self,
        generation: u64,
        parent_path: &str,
        entries: Vec<PathCompletionCandidate>,
    ) -> bool {
        if self.generation != generation || self.loading_parent.as_deref() != Some(parent_path) {
            return false;
        }
        self.loading_parent = None;
        self.loaded_parent = Some(parent_path.to_string());
        self.entries = entries;
        if self
            .request
            .as_ref()
            .map(|request| request.parent_path.as_str())
            == Some(parent_path)
        {
            self.rebuild_suggestions();
        }
        true
    }

    pub(super) fn dismiss(&mut self) {
        // Advancing the generation makes every in-flight remote result stale.
        self.generation = self.generation.wrapping_add(1);
        self.request = None;
        self.loaded_parent = None;
        self.loading_parent = None;
        self.entries.clear();
        self.suggestions.clear();
        self.selected_index = None;
        self.scroll_handle = ScrollHandle::new();
    }

    pub(super) fn is_visible(&self) -> bool {
        !self.suggestions.is_empty()
    }

    pub(super) fn suggestions(&self) -> &[PathCompletionCandidate] {
        &self.suggestions
    }

    pub(super) fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub(super) fn acceptance_index(&self, key: &str) -> Option<usize> {
        match key {
            // Suggestions arriving asynchronously must not change Enter's target.
            "enter" => self.selected_index,
            "tab" if self.is_visible() => Some(self.selected_index.unwrap_or(0)),
            _ => None,
        }
    }

    pub(super) fn candidate(&self, index: usize) -> Option<&PathCompletionCandidate> {
        self.suggestions.get(index)
    }

    pub(super) fn parent_path(&self) -> Option<&str> {
        self.request
            .as_ref()
            .map(|request| request.parent_path.as_str())
    }

    pub(super) fn move_selection(&mut self, delta: isize) -> bool {
        if self.suggestions.is_empty() {
            return false;
        }
        let max_index = self.suggestions.len().saturating_sub(1) as isize;
        let index = match self.selected_index {
            Some(index) => (index as isize + delta).clamp(0, max_index) as usize,
            None if delta < 0 => max_index as usize,
            None => 0,
        };
        self.selected_index = Some(index);
        // Keep keyboard navigation and the popup viewport owned by the same state.
        self.scroll_handle.scroll_to_item(index);
        true
    }

    fn rebuild_suggestions(&mut self) {
        let Some(request) = self.request.as_ref() else {
            self.suggestions.clear();
            return;
        };
        let prefix = request.prefix.to_lowercase();
        let mut suggestions = self
            .entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| {
            right
                .is_directory
                .cmp(&left.is_directory)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        // Bound pathological directories without confusing viewport height with result count.
        suggestions.truncate(PATH_COMPLETION_MAX_MATCHES);
        self.suggestions = suggestions;
        self.scroll_handle = ScrollHandle::new();
        self.selected_index = self
            .selected_index
            .filter(|index| *index < self.suggestions.len());
    }
}

pub(super) fn local_path_completion_request(input: &str) -> Option<PathCompletionRequest> {
    if input.trim().is_empty() {
        return None;
    }
    let normalized = oxide_local_files::resolve_home_path(input);
    split_path_completion_request(
        &normalized,
        input.ends_with(['/', '\\']),
        |character| character == '/' || character == '\\',
        ".",
    )
}

pub(super) fn remote_path_completion_request(input: &str) -> Option<PathCompletionRequest> {
    if input.trim().is_empty() {
        return None;
    }
    let normalized = oxide_sftp::normalize_remote_path(input);
    split_path_completion_request(
        &normalized,
        input.ends_with('/') || normalized == "/",
        |character| character == '/',
        "/",
    )
}

fn split_path_completion_request(
    normalized: &str,
    input_ends_with_separator: bool,
    is_separator: impl Fn(char) -> bool,
    relative_parent: &str,
) -> Option<PathCompletionRequest> {
    if normalized.is_empty() {
        return None;
    }
    if input_ends_with_separator {
        let parent_path = trim_completion_trailing_separators(normalized, &is_separator);
        return Some(PathCompletionRequest {
            parent_path,
            prefix: String::new(),
        });
    }

    let separator = normalized
        .char_indices()
        .rev()
        .find(|(_, character)| is_separator(*character));
    let Some((separator_index, separator_character)) = separator else {
        return Some(PathCompletionRequest {
            parent_path: relative_parent.to_string(),
            prefix: normalized.to_string(),
        });
    };
    let prefix_start = separator_index + separator_character.len_utf8();
    let raw_parent = &normalized[..separator_index];
    let parent_path = if raw_parent.is_empty() {
        separator_character.to_string()
    } else if raw_parent.len() == 2 && raw_parent.as_bytes().get(1) == Some(&b':') {
        format!("{raw_parent}{separator_character}")
    } else {
        raw_parent.to_string()
    };
    Some(PathCompletionRequest {
        parent_path,
        prefix: normalized[prefix_start..].to_string(),
    })
}

fn trim_completion_trailing_separators(path: &str, is_separator: &impl Fn(char) -> bool) -> String {
    let trimmed = path.trim_end_matches(is_separator);
    if trimmed.is_empty() {
        path.chars()
            .next()
            .map(|character| character.to_string())
            .unwrap_or_else(|| "/".to_string())
    } else if trimmed.len() == 2 && trimmed.as_bytes().get(1) == Some(&b':') {
        let separator = path.chars().last().unwrap_or(std::path::MAIN_SEPARATOR);
        format!("{trimmed}{separator}")
    } else {
        trimmed.to_string()
    }
}

impl WorkspaceApp {
    pub(super) fn render_path_completion_overlay(
        &self,
        owner: PathCompletionOwner,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (visible, selected_index, scroll_handle, suggestions) =
            self.path_completion_snapshot(owner, cx);
        if !visible {
            return None;
        }
        let anchor = self
            .text_input_anchors
            .get(owner.ime_target().anchor_id())?;
        let theme = self.theme_tokens.ui;
        let mut popup = div()
            .id(owner.popup_id())
            .w(anchor.bounds.size.width)
            .max_h(px(
                PATH_COMPLETION_ROW_HEIGHT * PATH_COMPLETION_VISIBLE_ROWS as f32
            ))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .rounded(px(self.theme_tokens.radii.md))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgba((theme.bg_elevated << 8) | PATH_COMPLETION_BG_ALPHA))
            .shadow_lg()
            // Deferred drawing alone does not prevent the file list behind the popup from hit-testing.
            .occlude();

        for (index, candidate) in suggestions.into_iter().enumerate() {
            let label = if candidate.is_directory {
                format!("{}/", candidate.name)
            } else {
                candidate.name.clone()
            };
            popup = popup.child(
                div()
                    .h(px(PATH_COMPLETION_ROW_HEIGHT))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .bg(if Some(index) == selected_index {
                        rgba((theme.bg_hover << 8) | PATH_COMPLETION_HOVER_ALPHA)
                    } else {
                        rgba(theme.bg_hover << 8)
                    })
                    .hover(move |row| {
                        row.bg(rgba((theme.bg_hover << 8) | PATH_COMPLETION_HOVER_ALPHA))
                    })
                    .child(Self::render_lucide_icon(
                        if candidate.is_directory {
                            LucideIcon::Folder
                        } else {
                            LucideIcon::File
                        },
                        13.0,
                        rgb(if candidate.is_directory {
                            theme.accent
                        } else {
                            theme.text_muted
                        }),
                    ))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.text))
                            .child(label),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.accept_path_completion(owner, index, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            );
        }

        Some(
            deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(anchor.bounds.bottom_left())
                    .offset(gpui::point(px(0.0), px(PATH_COMPLETION_POPUP_GAP)))
                    .position_mode(AnchoredPositionMode::Window)
                    .child(oxide_ui::modal::overlay_content_boundary(popup)),
            )
            .with_priority(oxide_ui::modal::TAURI_POPOVER_LAYER_PRIORITY)
            .into_any_element(),
        )
    }

    fn path_completion_snapshot(
        &self,
        owner: PathCompletionOwner,
        cx: &App,
    ) -> (
        bool,
        Option<usize>,
        ScrollHandle,
        Vec<PathCompletionCandidate>,
    ) {
        match owner {
            PathCompletionOwner::FileManager => {
                let state = &self.file_manager.read(cx).path_completion;
                (
                    state.is_visible(),
                    state.selected_index(),
                    state.scroll_handle.clone(),
                    state.suggestions().to_vec(),
                )
            }
            PathCompletionOwner::SftpLocal(surface) => {
                let _scope = self.enter_sftp_surface(surface);
                let sftp = self.sftp_view().read(cx);
                let state = &sftp.local_path_completion;
                (
                    state.is_visible(),
                    state.selected_index(),
                    state.scroll_handle.clone(),
                    state.suggestions().to_vec(),
                )
            }
            PathCompletionOwner::SftpRemote(surface) => {
                let _scope = self.enter_sftp_surface(surface);
                let sftp = self.sftp_view().read(cx);
                let state = &sftp.remote_path_completion;
                (
                    state.is_visible(),
                    state.selected_index(),
                    state.scroll_handle.clone(),
                    state.suggestions().to_vec(),
                )
            }
        }
    }

    fn accept_path_completion(
        &mut self,
        owner: PathCompletionOwner,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        match owner {
            PathCompletionOwner::FileManager => self.accept_file_manager_path_completion(index, cx),
            PathCompletionOwner::SftpLocal(surface) => {
                if !self.has_sftp_surface(surface) {
                    return;
                }
                let _scope = self.enter_sftp_surface(surface);
                self.accept_sftp_path_completion(sftp::SftpPane::Local, index, cx)
            }
            PathCompletionOwner::SftpRemote(surface) => {
                if !self.has_sftp_surface(surface) {
                    return;
                }
                let _scope = self.enter_sftp_surface(surface);
                self.accept_sftp_path_completion(sftp::SftpPane::Remote, index, cx)
            }
        }
    }
}
