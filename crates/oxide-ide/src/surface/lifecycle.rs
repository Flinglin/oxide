impl IdeSurface {
    pub fn new(
        fs: NodeAgentIdeFileSystem,
        tokens: ThemeTokens,
        labels: IdeLabels,
        runtime_settings: IdeRuntimeSettings,
        backend_runtime: Arc<tokio::runtime::Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
       // Node surfaces own independent scopes on the shared transport. A shell
        // surface adopts the exclusive PTY lease owner; later scoped clients
        // cannot shut down that owner's channel when they release themselves.
        let fs = if fs.is_bound() {
            fs
        } else {
            fs.scoped_owner()
        };
        Self {
            workspace: IdeWorkspace::new(),
            shell_return_requested: false,
            access_request: None,
            backend_binding_abort: None,
            agent_install_progress: None,
            agent_install_task: None,
            fs,
            tokens,
            labels,
            runtime_settings,
            focus_handle: cx.focus_handle(),
            modal_focus_handle: cx.focus_handle(),
            backend_runtime,
            load_state: IdeLoadState::Empty,
            node_id: None,
            root_path: None,
            git_branch: None,
            tree_width: IDE_TREE_DEFAULT_WIDTH,
            tree_resize: None,
            tree_resize_activation: None,
            generation: 0,
            editors: HashMap::new(),
            loading_paths: HashSet::new(),
            loading_file_tabs: HashSet::new(),
            saving_tabs: HashSet::new(),
            pending_saves: HashMap::new(),
            file_open_errors: HashMap::new(),
            save_after_close: None,
            conflict_state: None,
            pending_restore_files: Vec::new(),
            pending_restore_dirty_contents: BTreeMap::new(),
            pending_restore_file_formats: BTreeMap::new(),
            pending_reconnect_restore_node_id: None,
            pending_reconnect_restore_files_remaining: 0,
            last_error: None,
            folder_picker: FolderPickerState::default(),
            folder_switch_confirm_open: false,
            tree_rows_cache: None,
            tree_scroll_handle: UniformListScrollHandle::new(),
            tab_scroll_handle: ScrollHandle::new(),
            search: ProjectSearchState::default(),
            search_cache: HashMap::new(),
            search_cache_order: Vec::new(),
            pending_search_queries: BTreeMap::new(),
            pending_editor_reveals: BTreeMap::new(),
            tab_context_menu: None,
            tree_context_menu: None,
            tree_name_input: None,
            delete_confirm: None,
            tree_clipboard: None,
            tab_drag: None,
            agent_status_menu: None,
            file_format_menu: None,
            file_format_bounds: [None; 2],
            agent_status_trigger_bounds: None,
            agent_remove_confirm_open: false,
            agent_action: None,
            agent_refresh_origin: None,
            mount: IdeSurfaceMount::default(),
            agent_poll_generation: 0,
            agent_poll_task: None,
            agent_sampling_refresh_task: None,
            agent_sampling_backend_abort: None,
            agent_watch_generation: 0,
            watched_root_path: None,
            agent_watch_task: None,
            agent_watch_retry_task: None,
            agent_watch_stop_task: None,
            agent_watch_backend_abort: None,
            agent_watch_stop_in_flight: false,
            agent_watch_restart_requested: false,
            agent_watch_stop_generation: 0,
            agent_watch_stop_release_queue: None,
        }
    }

    pub fn load_state(&self) -> &IdeLoadState {
        &self.load_state
    }

    pub fn mount(&self) -> IdeSurfaceMount {
        self.mount
    }

    pub fn set_mount(&mut self, mount: IdeSurfaceMount, cx: &mut Context<Self>) {
        if self.mount == mount {
            return;
        }
        let was_visible = self.mount.is_visible();
        self.finish_tree_resize(cx);
        self.mount = mount;
        if mount.is_visible() {
            if !was_visible {
                // Mount visibility controls only page sampling. User operations
                // and node ownership continue independently while hidden.
                self.resume_agent_sampling(cx);
            }
        } else {
            self.cancel_agent_sampling();
            self.stop_agent_watch(cx);
        }
    }

    pub fn set_visual_and_runtime_settings(
        &mut self,
        tokens: ThemeTokens,
        runtime_settings: IdeRuntimeSettings,
        cx: &mut Context<Self>,
    ) {
        let previous_agent_mode = self.runtime_settings.agent_mode;
           let mut runtime_settings = runtime_settings;
        if self.fs.is_bound() {
            runtime_settings.agent_mode = if self.fs.access() == oxide_ide_fs::RemoteAccess::Sftp { NodeAgentMode::Disabled } else { NodeAgentMode::Enabled };
        }
        let next_agent_mode = runtime_settings.agent_mode;
        self.tokens = tokens;
        self.runtime_settings = runtime_settings;
        self.fs.set_mode(next_agent_mode);
        if next_agent_mode == NodeAgentMode::Disabled {
            self.cancel_agent_sampling();
            self.stop_agent_watch(cx);
        } else if previous_agent_mode == NodeAgentMode::Disabled && self.mount.is_visible() {
            self.resume_agent_sampling(cx);
        }

        for editor in self.editors.values() {
            apply_editor_runtime_settings(editor, self.tokens, &self.runtime_settings, cx);
        }
        cx.notify();
    }

    pub fn snapshot(&mut self, cx: &mut Context<Self>) -> Option<WorkspaceSnapshot> {
        self.sync_all_editors(cx);
        self.workspace.snapshot().ok()
    }

    pub fn reconnect_snapshot(&mut self, cx: &mut Context<Self>) -> Option<ReconnectIdeSnapshot> {
         if self.fs.terminal_session_id().is_some() {
            return None;
        }
        self.sync_all_editors(cx);
        let snapshot = self.workspace.snapshot().ok()?;
        let (connection_id, project_path) = match &snapshot.project.root {
            IdeLocation::Remote { node_id, path } => (node_id.clone(), path.clone()),
            IdeLocation::Local { .. } => return None,
        };
        let tab_paths = snapshot
            .tabs
            .iter()
            .filter_map(|tab| match &tab.location {
                IdeLocation::Remote { path, .. } => Some(path.clone()),
                IdeLocation::Local { .. } => None,
            })
            .collect::<Vec<_>>();
        let dirty_contents = snapshot
            .buffers
            .iter()
            .filter(|buffer| {
                buffer.revision != buffer.saved_revision
                    || buffer.text != buffer.saved_text
                    || buffer.format != buffer.saved_format
            })
            .filter_map(|buffer| match &buffer.location {
                IdeLocation::Remote { path, .. } => Some((path.clone(), buffer.text.to_string())),
                IdeLocation::Local { .. } => None,
            })
            .collect::<BTreeMap<_, _>>();

        Some(ReconnectIdeSnapshot {
            project_path,
            tab_paths,
            connection_id,
            dirty_contents,
            file_formats: snapshot
                .buffers
                .iter()
                .filter_map(|buffer| {
                    let IdeLocation::Remote { path, .. } = &buffer.location else {
                        return None;
                    };
                    Some((
                        path.clone(),
                        oxide_ssh::ReconnectIdeFileFormat {
                            encoding: buffer.format.encoding.clone(),
                            has_bom: buffer.format.has_bom,
                            line_ending: buffer.format.line_ending.label().into(),
                        },
                    ))
                })
                .collect(),
        })
    }

    pub fn open_remote_project(
        &mut self,
        node_id: impl Into<String>,
        root_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let node_id = node_id.into();
        let root_path = root_path.into();
        self.cancel_agent_sampling();
        if let Some(previous_node_id) = self.node_id.clone()
            && previous_node_id != node_id
        {
            self.stop_agent_watch(cx);
            self.release_ide_node_after_watch_stop(previous_node_id);
        } else if self.root_path.as_deref() != Some(root_path.as_str()) {
            self.stop_agent_watch(cx);
        }
        if self.pending_restore_files.is_empty() {
            self.pending_restore_dirty_contents.clear();
            self.pending_restore_file_formats.clear();
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.node_id = Some(node_id.clone());
        self.root_path = Some(root_path.clone());
        self.git_branch = None;
        self.load_state = IdeLoadState::Loading;
        self.last_error = None;
        self.conflict_state = None;
        self.loading_paths.clear();
        self.loading_file_tabs.clear();
        self.saving_tabs.clear();
          self.pending_saves.clear();
        self.file_open_errors.clear();
        self.tree_name_input = None;
        self.delete_confirm = None;
        self.tree_clipboard = None;
        self.agent_action = None;
        self.editors.clear();
        self.workspace = IdeWorkspace::new();
        cx.notify();

        let fs = self.fs.clone();
        let backend_runtime = self.backend_runtime.clone();
        let audit_request = self
            .pending_reconnect_restore_node_id
            .is_none()
            .then(|| {
                oxide_audit::AuditContext::current_request().or_else(|| {
                    oxide_audit::AuditContext::current().map(|mut context| {
                        context.source = oxide_audit::AuditSource::User;
                        context
                    })
                })
            })
            .flatten();
        cx.spawn(async move |weak, cx| {
            let result = await_ide_backend(backend_runtime.spawn(async move {
                let task = open_project_with_root_listing(fs, node_id, root_path);
                if let Some(context) = audit_request {
                    context.scope(task).await
                } else {
                    task.await
                }
            }))
            .await;
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(result) => this.apply_project_open(result, cx),
                    Err(error) => {
                        let message = error.message;
                        this.load_state = IdeLoadState::Error(message.clone());
                            if this.is_shell_ide() {
                            cx.emit(IdeSurfaceEvent::ShellOpenFailed);
                        }
                        if let Some(reconnect_node_id) =
                            this.pending_reconnect_restore_node_id.take()
                        {
                            cx.emit(IdeSurfaceEvent::ReconnectRestoreProjectFailed {
                                reconnect_node_id,
                                message,
                            });
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn open_remote_project_with_files(
        &mut self,
        node_id: impl Into<String>,
        root_path: impl Into<String>,
        file_paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.pending_restore_files = file_paths;
        self.pending_restore_dirty_contents.clear();
        self.pending_restore_file_formats.clear();
        self.open_remote_project(node_id, root_path, cx);
    }

    pub fn release_remote_session(&mut self, cx: &mut Context<Self>) {
         self.cancel_backend_binding();
        self.cancel_agent_sampling();
        self.stop_agent_watch(cx);
        self.clear_search_cache();
        self.search.generation = self.search.generation.wrapping_add(1);
        self.search.searching = false;
        self.pending_search_queries.clear();
        self.pending_reconnect_restore_node_id = None;
        self.pending_reconnect_restore_files_remaining = 0;
        if let Some(node_id) = self.node_id.take() {
            self.release_ide_node_after_watch_stop(node_id);
        }
    }

    fn release_ide_node_after_watch_stop(&mut self, node_id: String) {
        if self.agent_watch_stop_in_flight {
            // Keep the node consumer until watch/stop has used the existing
            // agent session; releasing first would turn cleanup into a no-op.
            if let Some(release_queue) = self.agent_watch_stop_release_queue.as_ref() {
                release_queue.request_release(&self.fs, node_id);
            } else {
                self.fs.release_ide_session_for_node(&node_id);
            }
        } else {
            self.fs.release_ide_session_for_node(&node_id);
        }
    }

    pub fn mark_connection_interrupted(&mut self, cx: &mut Context<Self>) {
        self.cancel_agent_sampling();
        self.stop_agent_watch(cx);
        self.clear_search_cache();
        self.search.generation = self.search.generation.wrapping_add(1);
        self.search.searching = false;
        self.pending_search_queries.clear();
        self.pending_reconnect_restore_node_id = None;
        self.pending_reconnect_restore_files_remaining = 0;
        if let Some(node_id) = self.node_id.clone() {
            self.release_ide_node_after_watch_stop(node_id);
        }
        if matches!(self.load_state, IdeLoadState::Ready) {
            self.load_state = IdeLoadState::Disconnected;
            cx.notify();
        }
    }

    pub fn restore_reconnect_snapshot(
        &mut self,
        snapshot: ReconnectIdeSnapshot,
        reconnect_node_id: String,
        cx: &mut Context<Self>,
 ) -> bool {
        self.restore_reconnect_with_access(snapshot, reconnect_node_id, self.fs.access(), cx)
    }

    /// Pending pickers and failed opens do not represent an opened project.
    pub fn has_open_project(&self) -> bool {
        self.root_path.is_some()
            && matches!(self.load_state, IdeLoadState::Ready | IdeLoadState::Disconnected)
    }

    pub fn remote_access(&self) -> oxide_ide_fs::RemoteAccess {
        self.fs.access()
    }

    pub fn restore_reconnect_with_access(
        &mut self,
        snapshot: ReconnectIdeSnapshot,
        reconnect_node_id: String,
        access: oxide_ide_fs::RemoteAccess,
        cx: &mut Context<Self>,
    ) -> bool {
        if access == oxide_ide_fs::RemoteAccess::Automatic {
            return self.apply_reconnect_snapshot(snapshot, reconnect_node_id, cx);
        }
        self.load_state = IdeLoadState::Loading;
        let fs = self.fs.clone();
        let runtime = self.backend_runtime.clone();
        let node = snapshot.connection_id.clone();
        self.cancel_backend_binding();
        let generation = self.generation;
        let progress = self.track_agent_install(access, cx);
        let job = runtime.spawn(async move { fs.bind_node(&node, access, progress).await });
        self.backend_binding_abort = Some(job.abort_handle());
        cx.spawn(async move |weak, cx| {
            let result = await_ide_backend(job).await;
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.backend_binding_abort = None;
                this.agent_install_task = None;
                this.agent_install_progress = None;
                match result {
                    Ok(fs) => {
                        this.runtime_settings.agent_mode = if fs.access() == oxide_ide_fs::RemoteAccess::Sftp { NodeAgentMode::Disabled } else { NodeAgentMode::Enabled };
                    this.fs = fs;
                        this.apply_reconnect_snapshot(snapshot, reconnect_node_id, cx);
                    }
                    Err(error) => {
                        this.load_state = IdeLoadState::Error(error.message.clone());
                        cx.emit(IdeSurfaceEvent::ReconnectRestoreProjectFailed {
                            reconnect_node_id,
                            message: error.message,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
        true
    }

    fn apply_reconnect_snapshot(
        &mut self, snapshot: ReconnectIdeSnapshot, reconnect_node_id: String, cx: &mut Context<Self>,
    ) -> bool {
        self.sync_all_editors(cx);
        let same_project_open = self.root_path.as_deref() == Some(snapshot.project_path.as_str())
            && self.node_id.as_deref() == Some(snapshot.connection_id.as_str());

        if self.root_path.is_some() && !same_project_open {
            return false;
        }

        self.pending_restore_dirty_contents = snapshot.dirty_contents;
        self.pending_restore_file_formats = snapshot
            .file_formats
            .into_iter()
            .filter_map(|(path, format)| {
                let line_ending = match format.line_ending.as_str() {
                    "LF" => LineEnding::Lf,
                    "CRLF" => LineEnding::CrLf,
                    "CR" => LineEnding::Cr,
                    _ => return None,
                };
                Some((
                    path,
                    TextFileFormat {
                        encoding: format.encoding,
                        has_bom: format.has_bom,
                        line_ending,
                    },
                ))
            })
            .collect();
        if same_project_open {
            self.load_state = IdeLoadState::Ready;
            self.last_error = None;
            self.resume_agent_sampling(cx);
            for path in snapshot.tab_paths {
                self.open_remote_file(
                    IdeLocation::remote(snapshot.connection_id.clone(), path),
                    None,
                    cx,
                );
            }
            cx.notify();
        } else {
            self.pending_reconnect_restore_node_id = Some(reconnect_node_id);
            self.pending_restore_files = snapshot.tab_paths;
            self.open_remote_project(snapshot.connection_id, snapshot.project_path, cx);
        }
        true
    }

    pub fn open_remote_folder_picker_for_node(
        &mut self,
        node_id: impl Into<String>,
        initial_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let node_id = node_id.into();
        let initial_path = normalize_remote_path(&initial_path.into());
        self.node_id = Some(node_id.clone());
        self.folder_picker.open = true;
        self.folder_picker.node_id = Some(node_id.clone());
        self.folder_picker.path_input_focused = true;
        self.load_folder_picker_path(node_id, initial_path, cx);
    }
}

impl Drop for IdeSurface {
    fn drop(&mut self) {
          self.cancel_backend_binding();
        if let Some(abort_handle) = self.agent_sampling_backend_abort.take() {
            abort_handle.abort();
        }
        if let Some(abort_handle) = self.agent_watch_backend_abort.take() {
            abort_handle.abort();
        }
        // GPUI can drop an IDE surface during workspace teardown without a
        // `Context`. Release only this surface's node because the file-system
        // registry is shared with other IDE surfaces and AI consumers.
        if let Some(node_id) = self.node_id.take() {
            if let Some(release_queue) = self.agent_watch_stop_release_queue.as_ref() {
                // The detached Tokio stop task owns the final remote cleanup
                // and consumer release even when no GPUI update can run.
                release_queue.request_release(&self.fs, node_id);
            } else {
                self.fs.release_ide_session_for_node(&node_id);
            }
        }
    }
}


impl IdeSurface {
    pub fn is_shell_ide(&self) -> bool {
        self.fs.terminal_session_id().is_some()
    }

    pub fn request_shell_return(&mut self, cx: &mut Context<Self>) {
        if !self.is_shell_ide() {
            return;
        }
        self.shell_return_requested = true;
        self.advance_shell_return(cx);
        cx.notify();
    }

    fn advance_shell_return(&mut self, cx: &mut Context<Self>) {
        if !self.shell_return_requested
            || self.workspace.pending_close().is_some()
            || !self.saving_tabs.is_empty()
        {
            return;
        }
        self.sync_all_editors(cx);
        while let Some(tab) = self.workspace.tabs().first().map(|tab| tab.id) {
            self.close_tab(tab, cx);
            if self.workspace.pending_close().is_some() {
                return;
            }
        }
        self.shell_return_requested = false;
        cx.emit(IdeSurfaceEvent::ShellReturnReady);
    }
}

impl IdeSurface {
    fn resume_backend_selection(&mut self, cx: &mut Context<Self>) {
        if self.access_request.is_some() {
            self.load_state = IdeLoadState::Empty;
            cx.notify();
        }
    }

    pub fn request_backend_selection(
        &mut self,
        node: String,
        path: String,
        cx: &mut Context<Self>,
    ) {
        self.node_id = Some(node.clone());
        self.access_request = Some((node, path));
        cx.notify();
    }

    fn select_ssh_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.generation;
        let prompt = window.prompt(
            gpui::PromptLevel::Info,
            &self.labels.access_modes[1],
            Some(&self.labels.agent_choice_hint),
            &[self.labels.agent_choices[0].as_str(), self.labels.agent_choices[1].as_str(), self.labels.cancel.as_str()],
            cx,
        );
        self.load_state = IdeLoadState::Loading;
        cx.spawn(async move |weak, cx| {
            let choice = prompt.await.unwrap_or(2);
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation || this.access_request.is_none() {
                    return;
                }
                match choice {
                    0 => this.select_access(oxide_ide_fs::RemoteAccess::Commands, cx),
                    1 => this.select_access(oxide_ide_fs::RemoteAccess::Agent, cx),
                    _ => this.resume_backend_selection(cx),
                }
            });
        }).detach();
        cx.notify();
    }

    pub fn select_access(&mut self, access: oxide_ide_fs::RemoteAccess, cx: &mut Context<Self>) {
        let Some((node, path)) = self.access_request.clone() else {
            return;
        };
        self.load_state = IdeLoadState::Loading;
        let fs = self.fs.clone();
        let runtime = self.backend_runtime.clone();
        self.cancel_backend_binding();
        let generation = self.generation;
        let node_for_job = node.clone();
        let progress = self.track_agent_install(access, cx);
        let job = runtime.spawn(async move { fs.bind_node(&node_for_job, access, progress).await });
        self.backend_binding_abort = Some(job.abort_handle());
        cx.spawn(async move |weak, cx| {
            let result = await_ide_backend(job).await;
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation { return; }
                this.backend_binding_abort = None;
                this.agent_install_task = None;
                this.agent_install_progress = None;
                match result {
                Ok(fs) => {
                    this.runtime_settings.agent_mode = if fs.access() == oxide_ide_fs::RemoteAccess::Sftp { NodeAgentMode::Disabled } else { NodeAgentMode::Enabled };
                    this.fs = fs;
                    this.access_request = None;
                    this.load_state = IdeLoadState::Empty;
                    this.open_remote_folder_picker_for_node(node, path, cx);
                }
                Err(error) => {
                    this.load_state = IdeLoadState::Empty;
                    this.last_error = Some(if access == oxide_ide_fs::RemoteAccess::Sftp {
                        this.labels.sftp_unavailable.clone()
                    } else if error.kind == IdeFileErrorKind::Unsupported {
                        this.labels.backend_requirements.clone()
                    } else {
                        this.labels.ssh_unavailable.clone()
                    });
                    cx.notify();
                }
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl IdeSurface {
    fn track_agent_install(
        &mut self,
        access: oxide_ide_fs::RemoteAccess,
        cx: &mut Context<Self>,
    ) -> Option<tokio::sync::watch::Sender<oxide_ide_fs::AgentInstallProgress>> {
        if access != oxide_ide_fs::RemoteAccess::Agent {
            return None;
        }
        let initial = oxide_ide_fs::AgentInstallProgress::Checking;
        let (sender, mut updates) = tokio::sync::watch::channel(initial);
        self.agent_install_progress = Some(initial);
        let generation = self.generation;
        // The binding owner cancels this observer together with its startup job.
        self.agent_install_task = Some(cx.spawn(async move |weak, cx| {
            while updates.changed().await.is_ok() {
                let progress = *updates.borrow_and_update();
                let Ok(current) = weak.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.agent_install_progress = Some(progress);
                    cx.notify();
                    true
                }) else {
                    break;
                };
                if !current {
                    break;
                }
            }
        }));
        Some(sender)
    }
    fn cancel_backend_binding(&mut self) {
        self.agent_install_task = None;
        self.agent_install_progress = None;
        self.generation = self.generation.wrapping_add(1);
        if let Some(abort) = self.backend_binding_abort.take() {
            abort.abort();
        }
    }
}
