// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

use std::{cell::RefCell, cmp::Ordering, sync::Arc};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    BufferOffset, EditTransaction, EditorError, LineCol, Selection, TextEdit, TextRange,
    line_index::LineIndex, piece_table::PieceTableTextBuffer,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryEntry {
    edits: Vec<TextEdit>,
    before_revision: u64,
    after_revision: u64,
}

/// Editable text buffer with line lookup, transactions, undo/redo, and dirty state.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    storage: PieceTableTextBuffer,
    text_cache: RefCell<Option<Arc<str>>>,
    line_starts: LineIndex,
    version: u64,
    content_revision: u64,
    saved_revision: u64,
    next_content_revision: u64,
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    changes: Option<Vec<BufferChange>>,
}

/// Immutable piece snapshot. Text allocations are shared; history and line indexes are not copied.
#[derive(Clone, Debug)]
pub struct TextSnapshot {
    storage: PieceTableTextBuffer,
}

impl TextSnapshot {
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    pub fn chunks(&self) -> impl Iterator<Item = &str> {
        self.storage.chunks()
    }

    /// Materialize only for a consumer whose API requires contiguous text.
    pub fn with_text<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        let mut chunks = self.chunks();
        let first = chunks.next().unwrap_or_default();
        if chunks.next().is_none() {
            f(first)
        } else {
            f(&self.storage.to_text())
        }
    }
}

/// Coordinate changes only; readers do not need another copy of edited text.
#[derive(Clone, Debug, PartialEq)]
pub struct BufferChange {
    pub before_version: u64,
    pub after_version: u64,
    pub replacements: Vec<(std::ops::Range<usize>, usize)>,
}

impl BufferChange {
    pub fn map_position(&self, position: f64) -> f64 {
        let mut shift = 0.0;
        for (range, length) in &self.replacements {
            if position < range.start as f64 {
                break;
            }
            if position <= range.end as f64 {
                return range.start as f64 + shift + *length as f64;
            }
            shift += *length as f64 - range.len() as f64;
        }
        (position + shift).max(0.0)
    }
}

impl TextBuffer {
    /// The subscribing editor must drain changes after observing a new version.
    pub fn track_changes(&mut self, enabled: bool) {
        self.changes = enabled.then(Vec::new);
    }

    pub fn take_changes(&mut self) -> Vec<BufferChange> {
        self.changes
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub fn new(text: impl Into<Arc<str>>) -> Self {
        let text = text.into();
        let line_starts = LineIndex::new(&text);
        let storage = PieceTableTextBuffer::new(text);
        let text_cache = RefCell::new(Some(storage.original.clone()));
        Self {
            storage,
            text_cache,
            line_starts,
            version: 0,
            content_revision: 0,
            saved_revision: 0,
            next_content_revision: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            changes: None,
        }
    }

    pub fn text(&self) -> String {
        self.with_text(str::to_string)
    }

    pub fn text_snapshot(&self) -> Arc<str> {
        self.with_text(|_| ());
        self.text_cache
            .borrow()
            .as_ref()
            .expect("text cache was materialized")
            .clone()
    }

    pub fn shared_snapshot(&self) -> TextSnapshot {
        TextSnapshot {
            storage: self.storage.clone(),
        }
    }

    /// Drop only the contiguous read cache; pieces and undo history retain ownership.
    pub fn release_text_cache(&self) {
        let mut cache = self.text_cache.borrow_mut();
        // Shared save/workspace snapshots already own these bytes. Keep their
        // identity instead of allocating a duplicate on the next read.
        if cache
            .as_ref()
            .is_some_and(|text| Arc::strong_count(text) == 1)
        {
            cache.take();
        }
    }

    pub fn with_text<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        if self.text_cache.borrow().is_none() {
            *self.text_cache.borrow_mut() = Some(self.storage.to_text().into());
        }
        let cache = self.text_cache.borrow();
        f(cache
            .as_deref()
            .expect("text cache should be materialized before callback"))
    }

    /// Contiguous access for one operation, without retaining a document-sized copy.
    pub fn with_temporary_text<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        let cache = self.text_cache.borrow();
        if let Some(text) = cache.as_deref() {
            f(text)
        } else {
            f(&self.storage.to_text())
        }
    }

    pub(crate) fn chunks(&self) -> impl Iterator<Item = &str> {
        self.storage.chunks()
    }

    pub fn utf16_offset(&self, byte: usize) -> usize {
        let mut remaining = byte.min(self.len());
        let mut units = 0;
        for chunk in self.chunks() {
            let end = crate::utf16::floor_char_boundary(chunk, remaining.min(chunk.len()));
            units += chunk[..end].encode_utf16().count();
            if remaining <= chunk.len() {
                break;
            }
            remaining -= chunk.len();
        }
        units
    }

    /// Offsets inside a surrogate pair resolve to its starting byte, as required by the editor adapter.
    pub fn byte_offset_for_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        let mut base = 0;
        for chunk in self.chunks() {
            if chunk.is_ascii() {
                if offset - units <= chunk.len() {
                    return base + offset - units;
                }
                units += chunk.len();
            } else {
                for (byte, ch) in chunk.char_indices() {
                    let next = units + ch.len_utf16();
                    if next > offset {
                        return base + byte;
                    }
                    if next == offset {
                        return base + byte + ch.len_utf8();
                    }
                    units = next;
                }
            }
            base += chunk.len();
        }
        self.len()
    }

    pub fn slice_utf16(&self, range: std::ops::Range<usize>) -> String {
        let start = self.byte_offset_for_utf16(range.start);
        let end = self.byte_offset_for_utf16(range.end);
        self.storage.slice_to_string(start..end)
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.content_revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.content_revision;
    }

    pub fn slice(&self, range: TextRange) -> Result<String, EditorError> {
        self.validate_range(range)?;
        Ok(self.storage.slice_to_string(range.as_range()))
    }

    pub fn line_text(&self, line: usize) -> Option<String> {
        let start = self.line_starts.get(line)?;
        let end = self.line_end_offset(line)?.0;
        Some(self.storage.slice_to_string(start..end))
    }

    pub fn word_at(&self, offset: BufferOffset) -> String {
        let byte = offset.0.min(self.len());
        let line = self.line_starts.line_for_offset(byte);
        let start = self.line_starts.get(line).unwrap();
        self.with_line_text(line, |text| {
            crate::word_at(text, BufferOffset(byte - start))
        })
        .unwrap_or_default()
    }

    pub fn with_line_text<R>(&self, line: usize, f: impl FnOnce(&str) -> R) -> Option<R> {
        let start = self.line_starts.get(line)?;
        let end = self.line_end_offset(line)?.0;
        if start == 0 && end == self.storage.len() {
            // A single-line document is already the requested row; cache it for repeated paints.
            return Some(self.with_text(f));
        }
        // A visible-row read must not rebuild the entire document after an edit.
        // Reuse a warm contiguous cache, otherwise materialize only this line.
        let cache = self.text_cache.borrow();
        if let Some(text) = cache.as_ref() {
            text.get(start..end).map(f)
        } else {
            drop(cache);
            Some(f(&self.storage.slice_to_string(start..end)))
        }
    }

    pub fn line_char_counts(&self) -> Vec<usize> {
        // Soft-wrap layout may scan every line after width changes. Count from
        // the materialized text cache so layout does not allocate one string per
        // line before it can rebuild display rows.
        self.with_text(|text| {
            (0..self.line_starts.len())
                .map(|line| {
                    let start = self.line_starts.get(line).unwrap();
                    let end = self
                        .line_starts
                        .get(line + 1)
                        .map(|next| next.saturating_sub(1))
                        .unwrap_or_else(|| text.len())
                        .max(start);
                    text.get(start..end)
                        .map(|line_text| line_text.chars().count())
                        .unwrap_or_default()
                })
                .collect()
        })
    }

    pub fn offset_to_line_col(&self, offset: BufferOffset) -> Result<LineCol, EditorError> {
        self.validate_offset(offset)?;
        let line = self.line_starts.line_for_offset(offset.0);
        Ok(LineCol::new(
            line,
            offset.0 - self.line_starts.get(line).unwrap(),
        ))
    }

    pub fn line_col_to_offset(&self, position: LineCol) -> Result<BufferOffset, EditorError> {
        let start = self
            .line_starts
            .get(position.line)
            .ok_or(EditorError::InvalidLine {
                line: position.line,
                line_count: self.line_count(),
            })?;
        let line_end = self
            .line_end_offset(position.line)
            .ok_or(EditorError::InvalidLine {
                line: position.line,
                line_count: self.line_count(),
            })?
            .0;
        let offset = start + position.column;
        if offset > line_end {
            return Err(EditorError::InvalidColumn {
                line: position.line,
                column: position.column,
                line_len: line_end - start,
            });
        }
        self.validate_offset(BufferOffset(offset))?;
        Ok(BufferOffset(offset))
    }

    pub fn line_start_offset(&self, line: usize) -> Option<BufferOffset> {
        self.line_starts.get(line).map(BufferOffset)
    }

    pub fn line_end_offset(&self, line: usize) -> Option<BufferOffset> {
        let start = self.line_starts.get(line)?;
        let next_start = self.line_starts.get(line + 1);
        let end = next_start
            .map(|next| next.saturating_sub(1))
            .unwrap_or_else(|| self.storage.len());
        Some(BufferOffset(end.max(start)))
    }

    pub fn next_grapheme_offset(&self, offset: BufferOffset) -> BufferOffset {
        if self.validate_offset(offset).is_err() || offset.0 >= self.storage.len() {
            return BufferOffset(self.storage.len());
        }
        let line = self.line_starts.line_for_offset(offset.0);
        let end = self.line_starts.get(line + 1).unwrap_or(self.len());
        let remaining = self.storage.slice_to_string(offset.0..end);
        let next_len = remaining.graphemes(true).next().map(str::len).unwrap_or(0);
        BufferOffset(offset.0 + next_len)
    }

    pub fn previous_grapheme_offset(&self, offset: BufferOffset) -> BufferOffset {
        if self.validate_offset(offset).is_err() || offset.0 == 0 {
            return BufferOffset::ZERO;
        }
        // Newlines terminate grapheme context, including regional-indicator runs.
        // Include the preceding physical line when the caret follows its newline.
        let line = self.line_starts.line_for_offset(offset.0 - 1);
        let start = self.line_starts.get(line).unwrap();
        let prefix = self.storage.slice_to_string(start..offset.0);
        let previous_len = prefix
            .graphemes(true)
            .next_back()
            .map(str::len)
            .unwrap_or(0);
        BufferOffset(offset.0.saturating_sub(previous_len))
    }

    pub fn apply_transaction(&mut self, transaction: EditTransaction) -> Result<(), EditorError> {
        if transaction.is_empty() {
            return Ok(());
        }
        let before_revision = self.content_revision;
        let after_revision = self.allocate_content_revision();
        let inverse = self.apply_edits_internal(transaction.into_edits())?;
        // Dirty tracking follows content revisions instead of the notification
        // version so undoing back to the saved content can clear the dirty flag.
        self.content_revision = after_revision;
        self.undo_stack.push(HistoryEntry {
            edits: inverse,
            before_revision,
            after_revision,
        });
        self.redo_stack.clear();
        Ok(())
    }

    pub fn replace_selection(
        &mut self,
        selection: Selection,
        replacement: impl Into<String>,
    ) -> Result<Selection, EditorError> {
        let range = selection.range();
        let replacement = replacement.into();
        let caret = BufferOffset(range.start.0 + replacement.len());
        self.apply_transaction(EditTransaction::single(TextEdit::new(range, replacement)))?;
        Ok(Selection::caret(caret))
    }

    pub fn undo(&mut self) -> Result<bool, EditorError> {
        let Some(entry) = self.undo_stack.pop() else {
            return Ok(false);
        };
        let redo = self.apply_edits_internal(entry.edits)?;
        // Undo restores the exact content revision that existed before the
        // transaction. This keeps dirty state independent from stack movement.
        self.content_revision = entry.before_revision;
        self.redo_stack.push(HistoryEntry {
            edits: redo,
            before_revision: entry.before_revision,
            after_revision: entry.after_revision,
        });
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, EditorError> {
        let Some(entry) = self.redo_stack.pop() else {
            return Ok(false);
        };
        let undo = self.apply_edits_internal(entry.edits)?;
        // Redo returns to the transaction's original after-revision instead of
        // minting a new one, preserving save/dirty parity with the first edit.
        self.content_revision = entry.after_revision;
        self.undo_stack.push(HistoryEntry {
            edits: undo,
            before_revision: entry.before_revision,
            after_revision: entry.after_revision,
        });
        Ok(true)
    }

    fn allocate_content_revision(&mut self) -> u64 {
        let revision = self.next_content_revision;
        self.next_content_revision = self.next_content_revision.saturating_add(1);
        revision
    }

    fn apply_edits_internal(&mut self, edits: Vec<TextEdit>) -> Result<Vec<TextEdit>, EditorError> {
        let edits = normalize_edits_for_storage(edits, &self.storage)?;
        if let Some(changes) = &mut self.changes {
            changes.push(BufferChange {
                before_version: self.version,
                after_version: self.version.saturating_add(1),
                replacements: edits
                    .iter()
                    .map(|edit| (edit.range.as_range(), edit.replacement.len()))
                    .collect(),
            });
        }
        let mut inverse = Vec::with_capacity(edits.len());
        let mut delta: isize = 0;

        for edit in &edits {
            let original = self.storage.slice_to_string(edit.range.as_range());
            let start_after = apply_delta(edit.range.start.0, delta)?;
            let end_after = start_after + edit.replacement.len();
            inverse.push(TextEdit::new(
                TextRange::new(BufferOffset(start_after), BufferOffset(end_after)),
                original,
            ));
            delta += edit.replacement.len() as isize - edit.range.len() as isize;
        }

        let new_len = apply_delta(self.len(), delta)?;
        self.line_starts.update(&edits, new_len);

        for edit in edits.iter().rev() {
            self.storage
                .replace(edit.range.as_range(), &edit.replacement);
        }
        // Syntax and save snapshots still require contiguous text. Invalidate
        // that version without forcing ordinary reads to materialize it again.
        *self.text_cache.borrow_mut() = None;
        self.storage.reclaim_unused_text();
        self.version = self.version.saturating_add(1);
        Ok(inverse)
    }

    fn validate_offset(&self, offset: BufferOffset) -> Result<(), EditorError> {
        if offset.0 > self.storage.len() {
            return Err(EditorError::OffsetOutOfBounds {
                offset: offset.0,
                len: self.storage.len(),
            });
        }
        if !self.storage.is_char_boundary(offset.0) {
            return Err(EditorError::InvalidUtf8Boundary { offset: offset.0 });
        }
        Ok(())
    }

    fn validate_range(&self, range: TextRange) -> Result<(), EditorError> {
        self.validate_offset(range.start)?;
        self.validate_offset(range.end)?;
        if range.start > range.end {
            return Err(EditorError::InvalidRange {
                start: range.start.0,
                end: range.end.0,
            });
        }
        Ok(())
    }
}

fn normalize_edits_for_storage(
    edits: Vec<TextEdit>,
    storage: &PieceTableTextBuffer,
) -> Result<Vec<TextEdit>, EditorError> {
    let mut edits = edits;
    edits.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then_with(|| left.range.end.cmp(&right.range.end))
    });

    let mut previous_end = BufferOffset::ZERO;
    for edit in &edits {
        validate_offset_for_storage(storage, edit.range.start)?;
        validate_offset_for_storage(storage, edit.range.end)?;
        if edit.range.start > edit.range.end {
            return Err(EditorError::InvalidRange {
                start: edit.range.start.0,
                end: edit.range.end.0,
            });
        }
        if edit.range.start < previous_end {
            return Err(EditorError::OverlappingEdits {
                offset: edit.range.start.0,
            });
        }
        previous_end = edit.range.end;
    }

    Ok(edits)
}

fn validate_offset_for_storage(
    storage: &PieceTableTextBuffer,
    offset: BufferOffset,
) -> Result<(), EditorError> {
    if offset.0 > storage.len() {
        return Err(EditorError::OffsetOutOfBounds {
            offset: offset.0,
            len: storage.len(),
        });
    }
    if !storage.is_char_boundary(offset.0) {
        return Err(EditorError::InvalidUtf8Boundary { offset: offset.0 });
    }
    Ok(())
}

fn apply_delta(offset: usize, delta: isize) -> Result<usize, EditorError> {
    match delta.cmp(&0) {
        Ordering::Less => offset
            .checked_sub(delta.unsigned_abs())
            .ok_or(EditorError::EditDeltaOverflow),
        Ordering::Equal => Ok(offset),
        Ordering::Greater => offset
            .checked_add(delta as usize)
            .ok_or(EditorError::EditDeltaOverflow),
    }
}
