use crate::TextEdit;

#[derive(Clone, Debug)]
pub(crate) enum LineIndex {
    Compact(Vec<u32>),
    Wide(Vec<usize>),
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> Self {
        if u32::try_from(text.len()).is_ok() {
            Self::Compact(compute_line_starts(text))
        } else {
            Self::Wide(compute_line_starts(text))
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Compact(starts) => starts.len(),
            Self::Wide(starts) => starts.len(),
        }
    }

    pub(crate) fn get(&self, line: usize) -> Option<usize> {
        match self {
            Self::Compact(starts) => starts.get(line).map(|&offset| offset as usize),
            Self::Wide(starts) => starts.get(line).copied(),
        }
    }

    pub(crate) fn line_for_offset(&self, offset: usize) -> usize {
        let next = match self {
            Self::Compact(starts) => starts.partition_point(|&start| start as usize <= offset),
            Self::Wide(starts) => starts.partition_point(|&start| start <= offset),
        };
        next.saturating_sub(1)
    }

    pub(crate) fn update(&mut self, edits: &[TextEdit], new_len: usize) {
        if u32::try_from(new_len).is_err()
            && let Self::Compact(starts) = self
        {
            *self = Self::Wide(starts.iter().map(|&offset| offset as usize).collect());
        }
        match self {
            Self::Compact(starts) => update_line_starts_after_edits(starts, edits),
            Self::Wide(starts) => update_line_starts_after_edits(starts, edits),
        }
    }
}

fn offset<T: Copy + TryInto<usize>>(value: T) -> usize {
    value.try_into().ok().expect("line offset fits usize")
}

fn stored_offset<T: TryFrom<usize>>(value: usize) -> T {
    T::try_from(value)
        .ok()
        .expect("line index width covers document")
}

fn compute_line_starts<T: TryFrom<usize>>(text: &str) -> Vec<T> {
    // Opening a file knows its exact line count; avoid geometric spare capacity
    // on the largest per-line allocation. Subsequent edits can grow normally.
    let mut starts = Vec::with_capacity(1 + text.bytes().filter(|&byte| byte == b'\n').count());
    starts.push(stored_offset(0));
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(stored_offset(index + 1));
        }
    }
    starts
}

fn update_line_starts_after_edits<T: Copy + TryFrom<usize> + TryInto<usize>>(
    starts: &mut Vec<T>,
    edits: &[TextEdit],
) {
    let Some(first_edit) = edits.first() else {
        return;
    };
    let first = starts.partition_point(|&value| offset(value) <= first_edit.range.start.0);
    let last = starts.partition_point(|&value| offset(value) <= edits.last().unwrap().range.end.0);
    let mut middle = Vec::new();
    let mut old_index = first;
    let mut shift: isize = 0;
    for edit in edits {
        while old_index < last && offset(starts[old_index]) <= edit.range.start.0 {
            push_line_start(
                &mut middle,
                apply_line_shift(offset(starts[old_index]), shift),
            );
            old_index += 1;
        }
        let base = apply_line_shift(edit.range.start.0, shift);
        for (index, byte) in edit.replacement.bytes().enumerate() {
            if byte == b'\n' {
                push_line_start(&mut middle, base + index + 1);
            }
        }
        while old_index < last && offset(starts[old_index]) <= edit.range.end.0 {
            old_index += 1;
        }
        shift += edit.replacement.len() as isize - edit.range.len() as isize;
    }
    let suffix = first + middle.len();
    // Preserve the unaffected prefix and reuse the allocation. Absolute offsets
    // after a byte-length change still need one shift, regardless of edit count.
    starts.splice(first..last, middle);
    if shift != 0 {
        for value in &mut starts[suffix..] {
            *value = stored_offset(apply_line_shift(offset(*value), shift));
        }
    }
    if starts.capacity() > starts.len().saturating_mul(2) {
        starts.shrink_to(starts.len());
    }
}

fn push_line_start<T: Copy + TryFrom<usize> + TryInto<usize>>(starts: &mut Vec<T>, value: usize) {
    if starts.last().copied().map(offset) != Some(value) {
        starts.push(stored_offset(value));
    }
}

fn apply_line_shift(offset: usize, shift: isize) -> usize {
    if shift < 0 {
        offset.saturating_sub(shift.unsigned_abs())
    } else {
        offset.saturating_add(shift as usize)
    }
}
