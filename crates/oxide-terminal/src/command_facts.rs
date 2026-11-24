use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use oxide_terminal_core::{
    TerminalCommandMark, TerminalCommandMarkClosedBy, TerminalCommandMarkConfidence,
    TerminalCommandMarkDetectionSource, terminal_autosuggest_fuzzy_score,
};
use parking_lot::Mutex;
use zeroize::Zeroize;

mod persistence;
pub use persistence::TerminalHistoryStore;

use crate::terminal_ui::MAX_HIGHLIGHT_PATTERN_LENGTH;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalCommandFactStatus {
    Open,
    Closed,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalCommandFact {
    pub fact_id: String,
    pub client_mark_id: String,
    pub source: TerminalCommandMarkDetectionSource,
    pub submitted_by: Option<TerminalCommandMarkDetectionSource>,
    pub command: Option<String>,
    pub start_global_line: usize,
    pub command_global_line: usize,
    pub output_start_global_line: usize,
    pub end_global_line: Option<usize>,
    pub status: TerminalCommandFactStatus,
    pub confidence: TerminalCommandMarkConfidence,
    pub closed_by: Option<TerminalCommandMarkClosedBy>,
    pub exit_code: Option<i32>,
    pub created_at: u64,
    pub closed_at: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalAiCommandRecord {
    pub command_id: String,
    pub command: String,
    pub source: TerminalCommandMarkDetectionSource,
    pub status: TerminalCommandFactStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub start_line: usize,
    pub end_line: Option<usize>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct TerminalAutosuggestCommandRecord {
    pub command_id: String,
    pub command: String,
    pub started_at: u64,
    pub finished_at: u64,
}

impl fmt::Debug for TerminalAutosuggestCommandRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalAutosuggestCommandRecord")
            .field("command_id", &self.command_id)
            // Command lines can contain credentials and must not enter diagnostic output.
            .field("command", &"<redacted>")
            .field("started_at", &self.started_at)
            .field("finished_at", &self.finished_at)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TerminalAutosuggestCandidate {
    pub(crate) command: String,
    pub(crate) use_count: usize,
    pub(crate) last_used_at: u64,
}

const MAX_AUTOSUGGEST_RECORDS: usize = 10_000;

#[derive(Clone, Default)]
pub struct SharedTerminalCommandHistory {
    state: Arc<Mutex<TerminalCommandHistoryState>>,
}

#[derive(Default)]
struct TerminalCommandHistoryState {
    records: Vec<TerminalAutosuggestCommandRecord>,
    persistence: Option<persistence::HistoryBinding>,
    persistent_ids: std::collections::HashSet<String>,
    removed_before_restore: std::collections::HashSet<String>,
    restore_cleared: bool,
    restored: bool,
    paused: bool,
}

impl Drop for TerminalCommandHistoryState {
    fn drop(&mut self) {
        for record in &mut self.records {
            record.command.zeroize();
        }
        for mut command in self.removed_before_restore.drain() {
            command.zeroize();
        }
    }
}

impl SharedTerminalCommandHistory {
    pub fn from_commands(commands: Vec<String>) -> Self {
        let now = now_millis();
        let command_count = commands.len();
        let records = commands
            .into_iter()
            .enumerate()
            .filter(|(_, command)| !command.trim().is_empty())
            .map(|(index, command)| {
                // Shell history is oldest-first, so retain that ordering when timestamps are absent.
                let used_at = now.saturating_sub(command_count.saturating_sub(index) as u64);
                TerminalAutosuggestCommandRecord {
                    command_id: format!("shell-history-{used_at}-{index}"),
                    command,
                    started_at: used_at,
                    finished_at: used_at,
                }
            })
            .collect::<Vec<_>>();
        let mut state = TerminalCommandHistoryState::default();
        state.records = records;
        trim_autosuggest_records(&mut state.records);
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn records(&self) -> Vec<TerminalAutosuggestCommandRecord> {
        self.state.lock().records.clone()
    }

    pub(crate) fn candidates(
        &self,
        state: &TerminalAutosuggestInputState,
        limit: usize,
    ) -> Vec<TerminalAutosuggestCandidate> {
        autosuggest_candidates_for_records(&self.state.lock().records, state, limit)
    }

    pub(crate) fn ghost_text(&self, state: &TerminalAutosuggestInputState) -> Option<String> {
        let query = state.value.as_str();
        self.candidates(state, 1)
            .into_iter()
            .next()
            .and_then(|candidate| candidate.command.strip_prefix(query).map(str::to_string))
            .filter(|suffix| !suffix.is_empty())
    }

    pub(crate) fn record(&self, command: &str) -> bool {
        if command.trim().is_empty() {
            return false;
        }
        let now = now_millis();
        let mut state = self.state.lock();
        if state.paused {
            return false;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let binding = state.persistence.clone();
        if binding.as_ref().is_some_and(|binding| binding.enabled()) {
            state.persistent_ids.insert(id.clone());
        }
        state.records.push(TerminalAutosuggestCommandRecord {
            command_id: id,
            command: command.to_string(),
            started_at: now,
            finished_at: now,
        });
        trim_autosuggest_records(&mut state.records);
        if state.persistent_ids.len() > MAX_AUTOSUGGEST_RECORDS {
            let retained = state
                .records
                .iter()
                .map(|record| record.command_id.clone())
                .collect::<std::collections::HashSet<_>>();
            state.persistent_ids.retain(|id| retained.contains(id));
        }
        drop(state);
        if let Some(binding) = binding.filter(|binding| binding.enabled()) {
            binding.changed();
        }
        true
    }

    pub fn remove(&self, command: &str) -> bool {
        let mut state = self.state.lock();
        let previous_len = state.records.len();
        let removed_ids = state
            .records
            .iter()
            .filter(|record| record.command == command)
            .map(|record| record.command_id.clone())
            .collect::<Vec<_>>();
        state.records.retain_mut(|record| {
            if record.command == command {
                record.command.zeroize();
                false
            } else {
                true
            }
        });
        for id in removed_ids {
            state.persistent_ids.remove(&id);
        }
        if !state.restored {
            state.removed_before_restore.insert(command.to_owned());
        }
        let binding = state.persistence.clone();
        let changed = state.records.len() != previous_len;
        drop(state);
        if let Some(binding) = binding {
            binding.changed();
        }
        changed
    }

    pub fn clear(&self) {
        let mut state = self.state.lock();
        for record in &mut state.records {
            record.command.zeroize();
        }
        state.records.clear();
        state.persistent_ids.clear();
        state.restore_cleared = true;
        let binding = state.persistence.clone();
        drop(state);
        if let Some(binding) = binding {
            binding.changed();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    pub fn set_paused(&self, paused: bool) {
        self.state.lock().paused = paused;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalAutosuggestInputState {
    pub value: String,
    pub cursor_index: usize,
    pub is_cursor_at_end: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TransientCommandHighlight {
    pub(crate) command_id: Arc<str>,
    pub(crate) query: Arc<str>,
    pub(crate) case_sensitive: bool,
    pub(crate) output_start_global_line: usize,
    pub(crate) output_end_global_line: Option<usize>,
}

#[derive(Debug, Eq, PartialEq)]
struct TransientLiteralQuery {
    query: String,
    case_sensitive: bool,
}

#[derive(Default)]
pub(crate) struct CommandFactLedger {
    audit: Option<oxide_audit::AuditContext>,
    audit_commands: HashMap<String, oxide_audit::AuditOperation>,
    facts: Vec<TerminalCommandFact>,
    ai_records: Vec<TerminalAiCommandRecord>,
    autosuggest_records: Vec<TerminalAutosuggestCommandRecord>,
    transient_command_highlight: Option<TransientCommandHighlight>,
}

impl CommandFactLedger {
    pub(crate) fn set_audit_context(&mut self, context: Option<oxide_audit::AuditContext>) {
        // In-flight guards retain their original connection snapshot.
        self.audit = context;
    }

    pub(crate) fn with_audit(context: Option<oxide_audit::AuditContext>) -> Self {
        Self {
            audit: context,
            ..Self::default()
        }
    }

    pub(crate) fn audit_enabled(&self) -> bool {
        self.audit.is_some()
    }

    pub(crate) fn record_dispatch(
        &self,
        command_id: Option<&str>,
        parent_id: Option<&str>,
        source: oxide_audit::AuditSource,
        sent: bool,
    ) {
        let Some(mut context) = self.audit.clone() else {
            return;
        };
        let request = oxide_audit::AuditContext::current_request();
        if let Some(request) = &request {
            context.agent_id = request.agent_id.clone();
            if context.parent_id.is_none() {
                context.parent_id = request.parent_id.clone();
            }
        }
        let request_source = request.map(|request| request.source);
        context.source = request_source
            .filter(|source| {
                matches!(
                    source,
                    oxide_audit::AuditSource::Ai
                        | oxide_audit::AuditSource::Mcp
                        | oxide_audit::AuditSource::Plugin
                        | oxide_audit::AuditSource::Cli
                )
            })
            .unwrap_or(source);
        let inherited_parent = context.parent_id.clone();
        context.parent_id = command_id
            .and_then(|id| self.audit_commands.get(id))
            .and_then(oxide_audit::AuditOperation::id)
            .map(str::to_string)
            .or_else(|| parent_id.map(str::to_string))
            .or(inherited_parent);
        context
            .operation(
                oxide_audit::AuditCategory::Automation,
                "command_dispatch",
                None,
            )
            .finish(
                if sent {
                    oxide_audit::AuditOutcome::Sent
                } else {
                    oxide_audit::AuditOutcome::Failed
                },
                oxide_audit::AuditEvidence::Dispatch,
                None,
                None,
            );
    }

    pub(crate) fn disable_audit(&mut self) {
        self.audit = None;
        self.audit_commands.clear();
    }

    pub(crate) fn interrupt_audit_commands(&mut self) {
        self.audit_commands.clear();
    }

    pub(crate) fn facts(&self) -> Vec<TerminalCommandFact> {
        self.facts.clone()
    }

    pub(crate) fn ai_records(&self) -> Vec<TerminalAiCommandRecord> {
        self.ai_records.clone()
    }

    pub(crate) fn ai_command_status(&self, id: &str) -> Option<TerminalCommandFactStatus> {
        self.ai_records
            .iter()
            .rev()
            .find(|record| record.command_id == id)
            .map(|record| record.status)
    }

    pub(crate) fn autosuggest_records(&self) -> Vec<TerminalAutosuggestCommandRecord> {
        self.autosuggest_records.clone()
    }

    pub(crate) fn transient_command_highlight(&self) -> Option<TransientCommandHighlight> {
        self.transient_command_highlight.clone()
    }

    pub(crate) fn autosuggest_ghost_text(
        &self,
        state: &TerminalAutosuggestInputState,
    ) -> Option<String> {
        let query = state.value.trim_start();
        if query.is_empty() || !state.is_cursor_at_end {
            return None;
        }
        self.autosuggest_records
            .iter()
            .rev()
            .find_map(|record| {
                (record.command.starts_with(query) && record.command != query)
                    .then(|| record.command[query.len()..].to_string())
            })
            .filter(|suffix| !suffix.is_empty())
    }

    pub(crate) fn remove_autosuggest_command(&mut self, command: &str) -> bool {
        let previous_len = self.autosuggest_records.len();
        self.autosuggest_records
            .retain(|record| record.command != command);
        self.autosuggest_records.len() != previous_len
    }

    pub(crate) fn record_runtime_autosuggest_command(&mut self, command: &str) {
        if command.trim().is_empty() {
            return;
        }
        let now = now_millis();
        self.autosuggest_records
            .push(TerminalAutosuggestCommandRecord {
                command_id: format!("runtime-autosuggest-{now}"),
                command: command.to_string(),
                started_at: now,
                finished_at: now,
            });
        trim_autosuggest_records(&mut self.autosuggest_records);
    }

    pub(crate) fn create_from_mark(&mut self, mark: &TerminalCommandMark) {
        if self
            .facts
            .iter()
            .any(|fact| fact.client_mark_id == mark.command_id)
        {
            return;
        }

        self.close_previous_open(mark.start_line);
        if let Some(context) = &self.audit {
            let mut context = context.clone();
            let request = oxide_audit::AuditContext::current_request();
            if let Some(request) = &request {
                context.agent_id = request.agent_id.clone();
                if context.parent_id.is_none() {
                    context.parent_id = request.parent_id.clone();
                }
            }
            let request_source = request.map(|request| request.source);
            context.source = request_source
                .filter(|source| {
                    matches!(
                        source,
                        oxide_audit::AuditSource::Ai
                            | oxide_audit::AuditSource::Mcp
                            | oxide_audit::AuditSource::Plugin
                            | oxide_audit::AuditSource::Cli
                    )
                })
                .unwrap_or(match mark.submitted_by.unwrap_or(mark.detection_source) {
                    TerminalCommandMarkDetectionSource::Ai => oxide_audit::AuditSource::Ai,
                    TerminalCommandMarkDetectionSource::Broadcast => {
                        oxide_audit::AuditSource::Broadcast
                    }
                    TerminalCommandMarkDetectionSource::CommandBar => {
                        oxide_audit::AuditSource::CommandBar
                    }
                    TerminalCommandMarkDetectionSource::QuickCommand => {
                        oxide_audit::AuditSource::QuickCommand
                    }
                    _ => oxide_audit::AuditSource::User,
                });
            self.audit_commands.insert(
                mark.command_id.clone(),
                context.operation(
                    oxide_audit::AuditCategory::Command,
                    "command_execute",
                    mark.command.as_deref(),
                ),
            );
        }
        // The pane owns one derived query for only the latest command fact. Replacing
        // it here prevents a prior grep query from leaking into later output.
        self.transient_command_highlight = mark
            .command
            .as_deref()
            .and_then(transient_literal_query)
            .map(|query| TransientCommandHighlight {
                command_id: Arc::from(mark.command_id.as_str()),
                query: Arc::from(query.query),
                case_sensitive: query.case_sensitive,
                output_start_global_line: mark.output_start_line(),
                output_end_global_line: None,
            });
        let fact = TerminalCommandFact {
            fact_id: format!("native-command-fact-{}", mark.command_id),
            client_mark_id: mark.command_id.clone(),
            source: mark.detection_source,
            submitted_by: mark.submitted_by,
            command: mark
                .command
                .clone()
                .filter(|command| !command.trim().is_empty()),
            start_global_line: mark.start_line,
            command_global_line: mark.command_line,
            output_start_global_line: mark.output_start_line(),
            end_global_line: None,
            status: TerminalCommandFactStatus::Open,
            confidence: mark.confidence,
            closed_by: None,
            exit_code: None,
            created_at: now_millis(),
            closed_at: None,
        };
        self.record_ai_command_if_eligible(mark, &fact);
        self.facts.push(fact);
    }

    pub(crate) fn trim_history(&mut self, lines: usize) {
        // Closed facts are historical metadata, not live grid annotations. Only
        // the latest open fact needs rebasing before its closing boundary arrives.
        if let Some(fact) = self.facts.last_mut()
            && fact.status == TerminalCommandFactStatus::Open
        {
            fact.start_global_line = fact.start_global_line.saturating_sub(lines);
            fact.command_global_line = fact.command_global_line.saturating_sub(lines);
            fact.output_start_global_line = fact.output_start_global_line.saturating_sub(lines);
            fact.end_global_line = fact.end_global_line.map(|end| end.saturating_sub(lines));
        }
        if let Some(highlight) = &mut self.transient_command_highlight {
            if highlight
                .output_end_global_line
                .is_some_and(|end| end < lines)
            {
                self.transient_command_highlight = None;
            } else {
                highlight.output_start_global_line =
                    highlight.output_start_global_line.saturating_sub(lines);
                highlight.output_end_global_line = highlight
                    .output_end_global_line
                    .map(|end| end.saturating_sub(lines));
            }
        }
    }

    pub(crate) fn reflow_visual_ranges(&mut self, marks: &[TerminalCommandMark]) {
        if let Some(fact) = self.facts.last_mut()
            && fact.status == TerminalCommandFactStatus::Open
            && let Some(mark) = marks
                .iter()
                .find(|mark| mark.command_id == fact.client_mark_id)
        {
            fact.start_global_line = mark.start_line;
            fact.command_global_line = mark.command_line;
            fact.output_start_global_line = mark.output_start_line();
            fact.end_global_line = mark.end_line;
        }
        if let Some(highlight) = &mut self.transient_command_highlight {
            if let Some(mark) = marks
                .iter()
                .find(|mark| mark.command_id == highlight.command_id.as_ref() && !mark.stale)
            {
                highlight.output_start_global_line = mark.output_start_line();
                highlight.output_end_global_line = mark.end_line;
            } else {
                self.transient_command_highlight = None;
            }
        }
    }
    pub(crate) fn close_from_mark(&mut self, mark: &TerminalCommandMark) {
        if let Some(operation) = self.audit_commands.remove(&mark.command_id) {
            use oxide_audit::{AuditEvidence, AuditOutcome};
            let evidence = if mark.closed_by == Some(TerminalCommandMarkClosedBy::ShellIntegration)
            {
                AuditEvidence::ShellIntegration
            } else {
                AuditEvidence::InputInference
            };
            let outcome = if evidence == AuditEvidence::ShellIntegration {
                match mark.exit_code {
                    Some(0) => AuditOutcome::Succeeded,
                    Some(_) => AuditOutcome::Failed,
                    None => AuditOutcome::Unknown,
                }
            } else {
                AuditOutcome::Unknown
            };
            operation.finish(outcome, evidence, mark.exit_code, None);
        }
        let mut closed_fact = None;
        if let Some(fact) = self
            .facts
            .iter_mut()
            .find(|fact| fact.client_mark_id == mark.command_id)
        {
            fact.end_global_line = Some(
                mark.end_line
                    .unwrap_or(mark.start_line)
                    .max(mark.start_line),
            );
            fact.status = if mark.stale {
                TerminalCommandFactStatus::Stale
            } else {
                TerminalCommandFactStatus::Closed
            };
            fact.closed_by = mark.closed_by;
            fact.exit_code = mark.exit_code;
            fact.closed_at = Some(mark.finished_at.unwrap_or_else(now_millis));
            closed_fact = Some(fact.clone());
        }

        if self
            .transient_command_highlight
            .as_ref()
            .is_some_and(|highlight| highlight.command_id.as_ref() == mark.command_id)
        {
            if mark.stale {
                self.transient_command_highlight = None;
            } else if let Some(highlight) = self.transient_command_highlight.as_mut() {
                highlight.output_end_global_line =
                    closed_fact.as_ref().and_then(|fact| fact.end_global_line);
            }
        }

        if let Some(fact) = closed_fact {
            self.record_ai_command_if_eligible(mark, &fact);
        }
    }

    fn close_previous_open(&mut self, next_start_line: usize) {
        for (_, operation) in self.audit_commands.drain() {
            operation.finish(
                oxide_audit::AuditOutcome::Unknown,
                oxide_audit::AuditEvidence::InputInference,
                None,
                None,
            );
        }
        let now = now_millis();
        for fact in &mut self.facts {
            if fact.status != TerminalCommandFactStatus::Open {
                continue;
            }
            fact.status = TerminalCommandFactStatus::Closed;
            fact.end_global_line = Some(
                next_start_line
                    .saturating_sub(1)
                    .max(fact.start_global_line),
            );
            fact.closed_by = Some(TerminalCommandMarkClosedBy::NextCommand);
            fact.closed_at = Some(now);
        }
    }

    fn record_ai_command_if_eligible(
        &mut self,
        mark: &TerminalCommandMark,
        fact: &TerminalCommandFact,
    ) {
        let Some(command) = mark
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return;
        };
        if fact.confidence != TerminalCommandMarkConfidence::High {
            return;
        }
        if !matches!(
            fact.source,
            TerminalCommandMarkDetectionSource::CommandBar
                | TerminalCommandMarkDetectionSource::Ai
                | TerminalCommandMarkDetectionSource::Broadcast
                | TerminalCommandMarkDetectionSource::ShellIntegration
        ) {
            return;
        }
        if let Some(record) = self
            .ai_records
            .iter_mut()
            .find(|record| record.command_id == mark.command_id)
        {
            // The opening record gives AI a stable identifier immediately;
            // closing the same fact fills in its authoritative status/exit code.
            record.status = fact.status;
            record.finished_at = mark.finished_at;
            record.exit_code = mark.exit_code;
            record.end_line = fact.end_global_line;
            return;
        }

        self.ai_records.push(TerminalAiCommandRecord {
            command_id: mark.command_id.clone(),
            command: command.to_string(),
            source: fact.source,
            status: fact.status,
            started_at: mark.started_at,
            finished_at: mark.finished_at,
            exit_code: mark.exit_code,
            start_line: mark.start_line,
            end_line: fact.end_global_line,
        });
        const MAX_AI_RECORDS: usize = 200;
        if self.ai_records.len() > MAX_AI_RECORDS {
            let overflow = self.ai_records.len() - MAX_AI_RECORDS;
            self.ai_records.drain(0..overflow);
        }
    }
}

fn transient_literal_query(command: &str) -> Option<TransientLiteralQuery> {
    // Keep the first stage intentionally conservative: unsupported shell syntax
    // or grep/rg options produce no transient highlight instead of a wrong one.
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let mut command_position = true;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        if is_shell_separator(token) {
            command_position = true;
            index += 1;
            continue;
        }
        if command_position {
            command_position = false;
            let executable = token.rsplit(['/', '\\']).next().unwrap_or(token);
            let executable = executable.strip_suffix(".exe").unwrap_or(executable);
            if executable.eq_ignore_ascii_case("grep") || executable.eq_ignore_ascii_case("rg") {
                return literal_query_after_command(&tokens[index + 1..]);
            }
        }
        index += 1;
    }
    None
}

fn autosuggest_candidates_for_records(
    records: &[TerminalAutosuggestCommandRecord],
    state: &TerminalAutosuggestInputState,
    limit: usize,
) -> Vec<TerminalAutosuggestCandidate> {
    let query = state.value.as_str();
    if query.trim().is_empty() || !state.is_cursor_at_end || limit == 0 {
        return Vec::new();
    }

    let mut candidates_by_command = HashMap::<&str, (f64, TerminalAutosuggestCandidate)>::new();
    for record in records {
        if record.command == query {
            continue;
        }
        let candidate = match candidates_by_command.entry(&record.command) {
            std::collections::hash_map::Entry::Occupied(entry) => &mut entry.into_mut().1,
            std::collections::hash_map::Entry::Vacant(entry) => {
                let score = terminal_autosuggest_fuzzy_score(&record.command, query);
                if score <= 0.0 {
                    continue;
                }
                &mut entry
                    .insert((
                        score,
                        TerminalAutosuggestCandidate {
                            command: record.command.clone(),
                            use_count: 0,
                            last_used_at: record.finished_at,
                        },
                    ))
                    .1
            }
        };
        candidate.use_count = candidate.use_count.saturating_add(1);
        candidate.last_used_at = candidate.last_used_at.max(record.finished_at);
    }

    let mut candidates = candidates_by_command.into_values().collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| right.use_count.cmp(&left.use_count))
            .then_with(|| right.last_used_at.cmp(&left.last_used_at))
            .then_with(|| left.command.cmp(&right.command))
    });
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

fn trim_autosuggest_records(records: &mut Vec<TerminalAutosuggestCommandRecord>) {
    if records.len() > MAX_AUTOSUGGEST_RECORDS {
        let overflow = records.len() - MAX_AUTOSUGGEST_RECORDS;
        for record in &mut records[..overflow] {
            record.command.zeroize();
        }
        records.drain(0..overflow);
    }
}

fn literal_query_after_command(tokens: &[&str]) -> Option<TransientLiteralQuery> {
    let mut case_sensitive = true;
    let mut fixed_strings = false;
    let mut options_ended = false;
    for token in tokens {
        if is_shell_separator(token) {
            return None;
        }
        if !options_ended && *token == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && token.starts_with("--") {
            match *token {
                "--ignore-case" => case_sensitive = false,
                "--case-sensitive" => case_sensitive = true,
                "--fixed-strings" => fixed_strings = true,
                "--line-number" | "--with-filename" | "--no-filename" | "--only-matching"
                | "--no-color" | "--text" | "--hidden" => {}
                "--invert-match" | "--regexp" | "--smart-case" => return None,
                option if option.starts_with("--color=") => {}
                _ => return None,
            }
            continue;
        }
        if !options_ended && token.starts_with('-') && token.len() > 1 {
            for flag in token[1..].chars() {
                match flag {
                    'i' => case_sensitive = false,
                    's' => case_sensitive = true,
                    'F' => fixed_strings = true,
                    'n' | 'H' | 'h' | 'o' | 'u' | 'a' => {}
                    'v' | 'e' | 'S' => return None,
                    _ => return None,
                }
            }
            continue;
        }

        let query = simple_query_token(token)?;
        if query.chars().count() > MAX_HIGHLIGHT_PATTERN_LENGTH
            || (!fixed_strings && query.chars().any(is_regex_meta_character))
        {
            return None;
        }
        let query = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        return Some(TransientLiteralQuery {
            query,
            case_sensitive,
        });
    }
    None
}

fn simple_query_token(token: &str) -> Option<&str> {
    if token.is_empty()
        || token
            .chars()
            .any(|ch| matches!(ch, '$' | '`' | '<' | '>' | '&' | ';'))
    {
        return None;
    }
    let bytes = token.as_bytes();
    if matches!(bytes.first(), Some(b'\'') | Some(b'"')) {
        let quote = *bytes.first()?;
        if bytes.len() < 2 || bytes.last().copied() != Some(quote) {
            return None;
        }
        let inner = &token[1..token.len() - 1];
        return (!inner.is_empty() && !inner.as_bytes().contains(&quote)).then_some(inner);
    }
    (!token.contains('\'') && !token.contains('"')).then_some(token)
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "|" | "||" | "&&" | ";")
}

fn is_regex_meta_character(ch: char) -> bool {
    matches!(
        ch,
        '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
