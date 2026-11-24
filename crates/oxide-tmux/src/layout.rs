use std::{error::Error, fmt};

use crate::{PaneId, ids::parse_decimal};

const MAX_LAYOUT_DEPTH: usize = 64;
const MAX_LAYOUT_NODES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutCell {
    pub width: u16,
    pub height: u16,
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    LeftRight,
    TopBottom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutKind {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        children: Vec<Layout>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    pub cell: LayoutCell,
    pub kind: LayoutKind,
}

impl Layout {
    pub fn parse(encoded: &[u8]) -> Result<Self, LayoutError> {
        let body = strip_checksum(encoded)?;
        let mut parser = LayoutParser {
            input: body,
            cursor: 0,
            nodes: 0,
        };
        let layout = parser.parse_node(0)?;
        if parser.cursor != body.len() {
            return Err(LayoutError::TrailingData);
        }
        Ok(layout)
    }

    pub fn panes(&self) -> impl Iterator<Item = (PaneId, LayoutCell)> + '_ {
        let mut pending = vec![self];
        std::iter::from_fn(move || {
            while let Some(node) = pending.pop() {
                match &node.kind {
                    LayoutKind::Pane(pane) => return Some((*pane, node.cell)),
                    LayoutKind::Split { children, .. } => pending.extend(children.iter().rev()),
                }
            }
            None
        })
    }

    pub fn pane_at(&self, x: usize, y: usize) -> Option<PaneId> {
        self.panes().find_map(|(pane, cell)| {
            let left = usize::from(cell.x);
            let top = usize::from(cell.y);
            let inside = x >= left
                && x < left.saturating_add(usize::from(cell.width))
                && y >= top
                && y < top.saturating_add(usize::from(cell.height));
            inside.then_some(pane)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    InvalidChecksum,
    InvalidNumber,
    InvalidSeparator,
    EmptySplit,
    UnterminatedSplit,
    TooDeep,
    TooManyNodes,
    TrailingData,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidChecksum => "tmux layout checksum is invalid",
            Self::InvalidNumber => "tmux layout contains an invalid number",
            Self::InvalidSeparator => "tmux layout contains an invalid separator",
            Self::EmptySplit => "tmux layout split has no children",
            Self::UnterminatedSplit => "tmux layout split is not terminated",
            Self::TooDeep => "tmux layout nesting is too deep",
            Self::TooManyNodes => "tmux layout contains too many nodes",
            Self::TrailingData => "tmux layout has trailing data",
        })
    }
}

impl Error for LayoutError {}

struct LayoutParser<'a> {
    input: &'a [u8],
    cursor: usize,
    nodes: usize,
}

impl LayoutParser<'_> {
    fn parse_node(&mut self, depth: usize) -> Result<Layout, LayoutError> {
        if depth > MAX_LAYOUT_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        self.nodes += 1;
        if self.nodes > MAX_LAYOUT_NODES {
            return Err(LayoutError::TooManyNodes);
        }

        let width = self.number_u16()?;
        self.expect(b'x')?;
        let height = self.number_u16()?;
        self.expect(b',')?;
        let x = self.number_u16()?;
        self.expect(b',')?;
        let y = self.number_u16()?;
        let cell = LayoutCell {
            width,
            height,
            x,
            y,
        };

        let separator = self.next().ok_or(LayoutError::InvalidSeparator)?;
        let kind = match separator {
            b',' => LayoutKind::Pane(PaneId(self.number_u64()?)),
            b'{' => LayoutKind::Split {
                direction: SplitDirection::LeftRight,
                children: self.children(b'}', depth + 1)?,
            },
            b'[' => LayoutKind::Split {
                direction: SplitDirection::TopBottom,
                children: self.children(b']', depth + 1)?,
            },
            _ => return Err(LayoutError::InvalidSeparator),
        };
        Ok(Layout { cell, kind })
    }

    fn children(&mut self, terminator: u8, depth: usize) -> Result<Vec<Layout>, LayoutError> {
        if self.peek() == Some(terminator) {
            return Err(LayoutError::EmptySplit);
        }
        let mut children = Vec::new();
        loop {
            children.push(self.parse_node(depth)?);
            match self.next() {
                Some(byte) if byte == terminator => return Ok(children),
                Some(b',') => {}
                Some(_) => return Err(LayoutError::InvalidSeparator),
                None => return Err(LayoutError::UnterminatedSplit),
            }
        }
    }

    fn number_u16(&mut self) -> Result<u16, LayoutError> {
        u16::try_from(self.number_u64()?).map_err(|_| LayoutError::InvalidNumber)
    }

    fn number_u64(&mut self) -> Result<u64, LayoutError> {
        let start = self.cursor;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.cursor += 1;
        }
        parse_decimal(&self.input[start..self.cursor]).ok_or(LayoutError::InvalidNumber)
    }

    fn expect(&mut self, expected: u8) -> Result<(), LayoutError> {
        if self.next() == Some(expected) {
            Ok(())
        } else {
            Err(LayoutError::InvalidSeparator)
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.cursor).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.cursor += 1;
        Some(byte)
    }
}

fn strip_checksum(encoded: &[u8]) -> Result<&[u8], LayoutError> {
    let Some(comma) = encoded.iter().position(|byte| *byte == b',') else {
        return Ok(encoded);
    };
    let checksum = &encoded[..comma];
    if checksum.len() != 4 || !checksum.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(encoded);
    }
    let expected = checksum.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    });
    let body = &encoded[comma + 1..];
    (expected == Some(layout_checksum(body)))
        .then_some(body)
        .ok_or(LayoutError::InvalidChecksum)
}

fn layout_checksum(body: &[u8]) -> u16 {
    body.iter().fold(0_u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}
