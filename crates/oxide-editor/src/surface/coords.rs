use std::ops::Range;

use oxide_editor_core::Selection;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn floor_char_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte = byte.saturating_sub(1);
    }
    byte
}

pub(super) fn selection_byte_range_for_line(
    selection: Selection,
    line_text: &str,
    line_range: Range<usize>,
) -> Option<Range<usize>> {
    if selection.is_caret() {
        return None;
    }
    let selected = selection.range();
    let start = selected.start.0.max(line_range.start);
    let end = selected.end.0.min(line_range.end);
    if start > end {
        return None;
    }
    if start == end {
        // A selected newline gives an empty physical line no text bytes, but
        // editors still paint one cell to keep a multi-line selection joined.
        return (line_text.is_empty()
            && selected.start.0 <= line_range.start
            && selected.end.0 > line_range.start)
            .then_some(0..0);
    }
    // Rendering uses shaped text positions, so preserve byte offsets instead
    // of converting through an assumed monospace cell width.
    let local_start = floor_char_boundary(line_text, start - line_range.start);
    let local_end = floor_char_boundary(line_text, end - line_range.start);
    (local_start < local_end).then_some(local_start..local_end)
}

pub(super) fn visual_column_for_byte_column(text: &str, byte_column: usize) -> usize {
    let byte_column = floor_char_boundary(text, byte_column);
    text.grapheme_indices(true)
        .take_while(|(start, grapheme)| start + grapheme.len() <= byte_column)
        .map(|(_, grapheme)| grapheme_visual_width(grapheme))
        .sum()
}

pub(super) fn byte_column_for_visual_column(text: &str, visual_column: usize) -> usize {
    let mut current_column = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        let next_column = current_column + grapheme_visual_width(grapheme);
        if visual_column < next_column {
            // A click inside a wide glyph resolves to its nearest legal caret boundary.
            return if visual_column.saturating_sub(current_column)
                < next_column.saturating_sub(visual_column)
            {
                start
            } else {
                end
            };
        }
        if visual_column == next_column {
            return end;
        }
        current_column = next_column;
    }
    text.len()
}

pub(super) fn grapheme_visual_width(grapheme: &str) -> usize {
    // Keep control-only graphemes addressable while matching conventional
    // monospace widths for CJK and emoji clusters.
    UnicodeWidthStr::width(grapheme).max(1)
}
