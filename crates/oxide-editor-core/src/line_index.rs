use crate::TextEdit;

pub(crate) fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

pub(crate) fn update_line_starts_after_edits(starts: &mut Vec<usize>, edits: &[TextEdit]) {
    let Some(first_edit) = edits.first() else {
        return;
    };
    let first = starts.partition_point(|&offset| offset <= first_edit.range.start.0);
    let last = starts.partition_point(|&offset| offset <= edits.last().unwrap().range.end.0);
    let mut middle = Vec::new();
    let mut old_index = first;
    let mut shift: isize = 0;
    for edit in edits {
        while old_index < last && starts[old_index] <= edit.range.start.0 {
            push_line_start(&mut middle, apply_line_shift(starts[old_index], shift));
            old_index += 1;
        }
        let base = apply_line_shift(edit.range.start.0, shift);
        for (index, byte) in edit.replacement.bytes().enumerate() {
            if byte == b'\n' {
                push_line_start(&mut middle, base + index + 1);
            }
        }
        while old_index < last && starts[old_index] <= edit.range.end.0 {
            old_index += 1;
        }
        shift += edit.replacement.len() as isize - edit.range.len() as isize;
    }
    let suffix = first + middle.len();
    // Preserve the unaffected prefix and reuse the allocation. Absolute offsets
    // after a byte-length change still need one shift, regardless of edit count.
    starts.splice(first..last, middle);
    if shift != 0 {
        for offset in &mut starts[suffix..] {
            *offset = apply_line_shift(*offset, shift);
        }
    }
}

fn push_line_start(starts: &mut Vec<usize>, offset: usize) {
    if starts.last().copied() != Some(offset) {
        starts.push(offset);
    }
}

fn apply_line_shift(offset: usize, shift: isize) -> usize {
    if shift < 0 {
        offset.saturating_sub(shift.unsigned_abs())
    } else {
        offset.saturating_add(shift as usize)
    }
}
