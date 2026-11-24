use std::{collections::HashMap, sync::Arc};

use alacritty_terminal::vte::ansi::CursorShape as AlacCursorShape;
pub use oxide_terminal_graphics::{
    GraphicsOptions, KittyFileTransmissionControl, TerminalImageAnimationState, TerminalImageData,
    TerminalImageFrame, TerminalImageId, TerminalImageProtocol,
};
pub use oxide_terminal_model::{
    TerminalAttrs, TerminalCell, TerminalColor, TerminalRow, TerminalStyleOrigin,
};

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub generation: u64,
    pub cols: usize,
    pub rows: usize,
    pub cursor_col: usize,
    pub cursor_row: usize,
    pub cursor_shape: TerminalCursorShape,
    pub display_offset: usize,
    pub scrollback_lines: usize,
    pub lines: Vec<TerminalRow>,
    pub images: Vec<TerminalImageSnapshot>,
}

impl TerminalSnapshot {
    pub fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    pub fn reuse_unchanged_rows_from(&mut self, previous: &Self) -> usize {
        let stable_positions = self.lines.len() == previous.lines.len()
            && self
                .lines
                .iter()
                .zip(&previous.lines)
                .all(|(row, previous_row)| row.line_id != 0 && row.line_id == previous_row.line_id);
        if stable_positions {
            return self
                .lines
                .iter_mut()
                .zip(&previous.lines)
                .filter_map(|(row, previous_row)| {
                    row.reuse_cells_from_if_equal(previous_row).then_some(())
                })
                .count();
        }

        // Stable line identities follow output rows as they move through the viewport. Raw
        // backend snapshots retain the grid-line fallback until the pane assigns identities.
        let previous_rows_by_id = previous
            .lines
            .iter()
            .filter(|row| row.line_id != 0)
            .map(|row| (row.line_id, row))
            .collect::<HashMap<_, _>>();
        let previous_rows_by_line = previous
            .lines
            .iter()
            .map(|row| (row.absolute_line, row))
            .collect::<HashMap<_, _>>();
        let mut reused_rows = 0;
        for row in &mut self.lines {
            let previous_row = if row.line_id != 0 {
                previous_rows_by_id.get(&row.line_id)
            } else {
                previous_rows_by_line.get(&row.absolute_line)
            };
            let Some(previous_row) = previous_row else {
                continue;
            };
            if row.reuse_cells_from_if_equal(previous_row) {
                reused_rows += 1;
            }
        }
        reused_rows
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalImageSnapshot {
    pub id: TerminalImageId,
    pub protocol: TerminalImageProtocol,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub z_index: i32,
    pub placeholder: bool,
    pub version: u64,
    pub data: Option<Arc<TerminalImageData>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSearchRange {
    pub line: i32,
    pub start_col: usize,
    pub end_col: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSearchMatch {
    pub line: i32,
    pub start_col: usize,
    pub end_col: usize,
    pub ranges: Vec<TerminalSearchRange>,
}

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum TerminalCursorShape {
    #[default]
    Block,
    Underline,
    Bar,
    Hollow,
    Hidden,
}

impl From<AlacCursorShape> for TerminalCursorShape {
    fn from(value: AlacCursorShape) -> Self {
        match value {
            AlacCursorShape::Block => TerminalCursorShape::Block,
            AlacCursorShape::Underline => TerminalCursorShape::Underline,
            AlacCursorShape::Beam => TerminalCursorShape::Bar,
            AlacCursorShape::HollowBlock => TerminalCursorShape::Hollow,
            AlacCursorShape::Hidden => TerminalCursorShape::Hidden,
        }
    }
}
