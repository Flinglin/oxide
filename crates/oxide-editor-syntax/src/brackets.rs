use oxide_editor_core::BufferOffset;

use crate::BracketPair;

pub(crate) fn bracket_pairs(source: &str) -> Vec<BracketPair> {
    bracket_pairs_controlled(source, None).expect("uncontrolled bracket scans cannot be cancelled")
}

pub(crate) fn bracket_pairs_controlled(
    source: &str,
    work: Option<&crate::SyntaxWork>,
) -> Result<Vec<BracketPair>, crate::SyntaxError> {
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut pairs = Vec::new();

    for (index, byte) in source.bytes().enumerate() {
        if index % 4096 == 0 {
            crate::work::checkpoint(work)?;
        }
        match byte {
            b'(' | b'[' | b'{' => stack.push((byte, index)),
            b')' | b']' | b'}' => {
                let Some(position) = stack
                    .iter()
                    .rposition(|(open, _)| brackets_match(*open, byte))
                else {
                    continue;
                };
                let (_, open_index) = stack.remove(position);
                pairs.push(BracketPair {
                    open: BufferOffset(open_index),
                    close: BufferOffset(index),
                });
            }
            _ => {}
        }
    }

    pairs.sort_by_key(|pair| pair.open);
    crate::work::checkpoint(work)?;
    Ok(pairs)
}

fn brackets_match(open: u8, close: u8) -> bool {
    matches!((open, close), (b'(', b')') | (b'[', b']') | (b'{', b'}'))
}

/// Keep each pair once. Two sorted endpoints replace four hash-map entries.
#[derive(Debug, Default)]
pub struct BracketIndex {
    pairs: Box<[BracketPair]>,
    by_close: Box<[usize]>,
}

impl BracketIndex {
    pub(crate) fn new(
        pairs: Vec<BracketPair>,
        work: Option<&crate::SyntaxWork>,
    ) -> Result<Self, crate::SyntaxError> {
        crate::work::checkpoint(work)?;
        let mut by_close: Vec<_> = (0..pairs.len()).collect();
        by_close.sort_unstable_by_key(|&index| pairs[index].close.0);
        crate::work::checkpoint(work)?;
        Ok(Self {
            pairs: pairs.into_boxed_slice(),
            by_close: by_close.into_boxed_slice(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn pair_at(&self, caret: usize) -> Option<&BracketPair> {
        let mut first = None;
        for position in [Some(caret), caret.checked_sub(1)].into_iter().flatten() {
            if let Ok(index) = self
                .pairs
                .binary_search_by_key(&position, |pair| pair.open.0)
            {
                first = Some(first.map_or(index, |old: usize| old.min(index)));
            }
            if let Ok(index) = self
                .by_close
                .binary_search_by_key(&position, |&index| self.pairs[index].close.0)
            {
                let index = self.by_close[index];
                first = Some(first.map_or(index, |old: usize| old.min(index)));
            }
        }
        // Adjacent/nested brackets can claim the same caret slot. The old map
        // kept the pair with the earliest opening byte; preserve that priority.
        first.map(|index| &self.pairs[index])
    }
}
