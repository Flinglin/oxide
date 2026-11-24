use std::collections::VecDeque;

use zeroize::Zeroizing;

use super::{WorkspaceImeSelection, WorkspaceImeTarget};

const MAX_EDITS: usize = 128;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct InputSnapshot {
    pub text: Zeroizing<String>,
    pub selection: WorkspaceImeSelection,
}

struct InputEdit {
    target: WorkspaceImeTarget,
    before: InputSnapshot,
    after: InputSnapshot,
}

impl InputEdit {
    fn bytes(&self) -> usize {
        self.before.text.len() + self.after.text.len()
    }
}

/// Window-owned, bounded history. Drafts may contain credentials even in plain inputs.
#[derive(Default)]
pub(super) struct InputHistory {
    undo: VecDeque<InputEdit>,
    redo: VecDeque<InputEdit>,
}

impl InputHistory {
    pub fn record(&mut self, before: InputSnapshot, after: InputSnapshot) {
        if before.text == after.text {
            return;
        }
        let target = before.selection.target;
        // A programmatic replacement starts a new history for this input only.
        if self
            .undo
            .iter()
            .rfind(|edit| edit.target == target)
            .is_some_and(|edit| edit.after.text != before.text)
        {
            self.clear(target);
        }
        self.redo.retain(|edit| edit.target != target);
        self.undo.push_back(InputEdit {
            target,
            before,
            after,
        });
        while self.undo.len() + self.redo.len() > MAX_EDITS
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(InputEdit::bytes)
                .sum::<usize>()
                > MAX_TEXT_BYTES
        {
            if self.undo.pop_front().is_none() {
                self.redo.pop_front();
            }
        }
    }

    pub fn restore(
        &mut self,
        target: WorkspaceImeTarget,
        current: &str,
        redo: bool,
    ) -> Option<InputSnapshot> {
        let source = if redo { &mut self.redo } else { &mut self.undo };
        let index = source.iter().rposition(|edit| edit.target == target)?;
        let edit = source.remove(index)?;
        let (expected, restored) = if redo {
            (&edit.before, &edit.after)
        } else {
            (&edit.after, &edit.before)
        };
        if expected.text.as_str() != current {
            self.clear(target);
            return None;
        }
        let snapshot = InputSnapshot {
            text: restored.text.clone(),
            selection: restored.selection.clone(),
        };
        if redo {
            self.undo.push_back(edit);
        } else {
            self.redo.push_back(edit);
        }
        Some(snapshot)
    }

    fn clear(&mut self, target: WorkspaceImeTarget) {
        self.clear_matching(|candidate| candidate == target);
    }

    pub fn clear_matching(&mut self, matches: impl Fn(WorkspaceImeTarget) -> bool) {
        self.undo.retain(|edit| !matches(edit.target));
        self.redo.retain(|edit| !matches(edit.target));
    }
}
