use std::path::Path;

use super::{
    QUICK_COMMANDS_SCHEMA_VERSION, QuickCommand, QuickCommandAvailability,
    QuickCommandCategoryDraft, QuickCommandEditorDraft, QuickCommandImportResult,
    QuickCommandImportStrategy, QuickCommandsSnapshot, QuickCommandsState,
    default_quick_command_categories, default_quick_commands, now_ms,
};
use oxide_quick_commands::{
    QuickCommandDraft, delete_quick_command, delete_quick_command_category,
    ensure_active_quick_command_category, new_quick_command_id, upsert_quick_command,
    upsert_quick_command_category, visible_quick_commands, visible_quick_commands_for_management,
};

impl QuickCommandsState {
    pub(in crate::workspace) fn load(settings_path: &Path) -> Self {
        let mut state = Self {
            settings_path: settings_path.to_path_buf(),
            categories: default_quick_command_categories(),
            commands: default_quick_commands(),
            active_category: "system".to_string(),
            query: String::new(),
            focused_input: None,
            highlighted_command: None,
            command_editor: None,
            category_editor: None,
            last_persist_error: None,
        };

        match oxide_quick_commands::load_snapshot(settings_path) {
            Ok(snapshot) => {
                state.categories = snapshot.categories;
                state.commands = snapshot.commands;
                state.ensure_active_category();
            }
            Err(error) => {
                state.last_persist_error = Some(error);
            }
        }
        state
    }

    pub(super) fn visible_commands_for_targets(
        &self,
        target: Option<&oxide_quick_commands::QuickCommandTargetContext>,
    ) -> Vec<QuickCommand> {
        visible_quick_commands(&self.commands, &self.active_category, &self.query, target)
    }

    pub(super) fn visible_commands_for_management(&self) -> Vec<QuickCommand> {
        visible_quick_commands_for_management(&self.commands, &self.active_category, &self.query)
    }

    pub(in crate::workspace) fn upsert_command(&mut self, draft: QuickCommandDraft) {
        let updated_at = now_ms();
        let mut commands = self.commands.clone();
        if !upsert_quick_command(&mut commands, &self.categories, draft, updated_at) {
            return;
        }
        // Plugin-originated candidates become visible only after full snapshot validation succeeds.
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories: self.categories.clone(),
            commands: commands.clone(),
            updated_at,
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.commands = commands;
                self.last_persist_error = None;
            }
            Err(error) => self.last_persist_error = Some(error),
        }
    }

    pub(in crate::workspace) fn upsert_editor_command(
        &mut self,
        draft: QuickCommandEditorDraft,
    ) -> bool {
        let updated_at = now_ms();
        let command = QuickCommand {
            id: draft.id.unwrap_or_else(new_quick_command_id),
            name: draft.name.trim().to_string(),
            command: draft.command.trim().to_string(),
            category: if self
                .categories
                .iter()
                .any(|category| category.id == draft.category)
            {
                draft.category
            } else {
                "custom".to_string()
            },
            description: trimmed_optional(draft.description),
            parameters: draft
                .parameters
                .into_iter()
                .map(|parameter| super::QuickCommandParameter {
                    name: parameter.name.trim().to_string(),
                    label: parameter.label.trim().to_string(),
                    kind: parameter.kind,
                    default_value: (parameter.kind != super::QuickCommandParameterKind::Secret)
                        .then(|| trimmed_optional(parameter.default_value))
                        .flatten(),
                    choices: if parameter.kind == super::QuickCommandParameterKind::Secret {
                        Vec::new()
                    } else {
                        split_choices(&parameter.choices)
                    },
                    required: parameter.required,
                })
                .collect(),
            availability: QuickCommandAvailability {
                match_field: draft.match_field,
                protocols: draft.protocols,
                host_patterns: split_host_patterns(&draft.host_patterns),
            },
            confirmation: draft.confirmation,
            sort_order: draft.sort_order,
            created_at: draft.created_at,
            updated_at,
        };
        if command.name.is_empty() || command.command.is_empty() {
            return false;
        }
        // Persist the candidate snapshot before exposing it to readers so a
        // validation or I/O failure cannot leave unsaved editor state active.
        let mut commands = self.commands.clone();
        if let Some(existing) = commands
            .iter_mut()
            .find(|existing| existing.id == command.id)
        {
            *existing = command;
        } else {
            commands.push(command);
        }
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories: self.categories.clone(),
            commands: commands.clone(),
            updated_at,
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.commands = commands;
                self.last_persist_error = None;
                true
            }
            Err(error) => {
                self.last_persist_error = Some(error);
                false
            }
        }
    }

    pub(in crate::workspace) fn delete_command(&mut self, id: &str) {
        if delete_quick_command(&mut self.commands, id) {
            self.persist();
        }
    }

    pub(in crate::workspace) fn move_command(&mut self, id: &str, offset: isize) -> bool {
        if offset == 0 {
            return false;
        }
        let Some(category) = self
            .commands
            .iter()
            .find(|command| command.id == id)
            .map(|command| command.category.clone())
        else {
            return false;
        };
        let mut category_indices = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, command)| command.category == category)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        category_indices.sort_by_key(|index| self.commands[*index].sort_order);
        let Some(current_position) = category_indices
            .iter()
            .position(|index| self.commands[*index].id == id)
        else {
            return false;
        };
        let target_position = current_position.saturating_add_signed(offset);
        if target_position >= category_indices.len() || target_position == current_position {
            return false;
        }
        category_indices.swap(current_position, target_position);

        let updated_at = now_ms();
        let mut commands = self.commands.clone();
        // Normalize only this category so duplicate imported sort keys cannot block a move.
        for (sort_order, index) in category_indices.into_iter().enumerate() {
            commands[index].sort_order = sort_order as i64;
            commands[index].updated_at = updated_at;
        }
        commands.sort_by_key(|command| command.sort_order);
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories: self.categories.clone(),
            commands: commands.clone(),
            updated_at,
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.commands = commands;
                if let Some(editor) = self.command_editor.as_mut()
                    && let Some(editor_id) = editor.id.as_deref()
                    && let Some(command) =
                        self.commands.iter().find(|command| command.id == editor_id)
                {
                    editor.sort_order = command.sort_order;
                }
                self.last_persist_error = None;
                true
            }
            Err(error) => {
                self.last_persist_error = Some(error);
                false
            }
        }
    }

    pub(super) fn upsert_category(&mut self, draft: QuickCommandCategoryDraft) -> String {
        let mut categories = self.categories.clone();
        let active_category =
            upsert_quick_command_category(&mut categories, draft, &self.active_category);
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories,
            commands: self.commands.clone(),
            updated_at: now_ms(),
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.categories = snapshot.categories;
                self.active_category = active_category;
                self.last_persist_error = None;
            }
            Err(error) => self.last_persist_error = Some(error),
        }
        self.active_category.clone()
    }

    pub(super) fn delete_category(&mut self, id: &str) -> bool {
        let mut categories = self.categories.clone();
        let mut commands = self.commands.clone();
        if !delete_quick_command_category(&mut categories, &mut commands, id) {
            return false;
        }
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories,
            commands,
            updated_at: now_ms(),
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.categories = snapshot.categories;
                self.commands = snapshot.commands;
                self.ensure_active_category();
                self.last_persist_error = None;
                true
            }
            Err(error) => {
                self.last_persist_error = Some(error);
                false
            }
        }
    }

    pub(super) fn move_category(&mut self, source_id: &str, target_id: &str) -> bool {
        let Some(source) = self
            .categories
            .iter()
            .position(|category| category.id == source_id)
        else {
            return false;
        };
        let Some(target) = self
            .categories
            .iter()
            .position(|category| category.id == target_id)
        else {
            return false;
        };
        if source == target {
            return false;
        }
        let mut categories = self.categories.clone();
        let category = categories.remove(source);
        categories.insert(target, category);
        for (index, category) in categories.iter_mut().enumerate() {
            category.sort_order = index as i64;
        }
        let snapshot = QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories,
            commands: self.commands.clone(),
            updated_at: now_ms(),
        };
        match oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot) {
            Ok(()) => {
                self.categories = snapshot.categories;
                self.last_persist_error = None;
                true
            }
            Err(error) => {
                self.last_persist_error = Some(error);
                false
            }
        }
    }

    pub(in crate::workspace) fn export_snapshot_json(&self) -> Result<String, String> {
        oxide_quick_commands::export_snapshot_json(&self.settings_path)
    }

    pub(in crate::workspace) fn apply_snapshot_json(
        &mut self,
        snapshot_json: &str,
        strategy: QuickCommandImportStrategy,
    ) -> QuickCommandImportResult {
        let result =
            oxide_quick_commands::apply_snapshot_json(&self.settings_path, snapshot_json, strategy);
        if result.errors.is_empty() {
            self.reload_from_store();
        }
        result
    }

    pub(in crate::workspace) fn reload_from_store(&mut self) {
        match oxide_quick_commands::load_snapshot(&self.settings_path) {
            Ok(snapshot) => {
                self.categories = snapshot.categories;
                self.commands = snapshot.commands;
                self.ensure_active_category();
                self.highlighted_command = self
                    .highlighted_command
                    .take()
                    .filter(|id| self.commands.iter().any(|command| command.id == *id));
                self.last_persist_error = None;
            }
            Err(error) => self.last_persist_error = Some(error),
        }
    }

    fn snapshot(&self) -> QuickCommandsSnapshot {
        QuickCommandsSnapshot {
            version: QUICK_COMMANDS_SCHEMA_VERSION,
            categories: self.categories.clone(),
            commands: self.commands.clone(),
            updated_at: now_ms(),
        }
    }

    fn persist(&mut self) {
        let snapshot = self.snapshot();
        self.last_persist_error =
            oxide_quick_commands::save_snapshot(&self.settings_path, &snapshot).err();
    }

    fn ensure_active_category(&mut self) {
        ensure_active_quick_command_category(&self.categories, &mut self.active_category);
    }
}

fn trimmed_optional(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn split_host_patterns(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_string)
        .collect()
}

fn split_choices(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|choice| !choice.is_empty())
        .map(str::to_string)
        .collect()
}
