//! Paint-ready terminal row data shared by runtime and presentation crates.
//!
//! This crate deliberately contains no terminal transport or PTY behavior so
//! Unicode layout and other readers can consume snapshots without importing
//! the complete terminal runtime dependency graph.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct TerminalCell {
    pub ch: char,
    pub wide: bool,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub style_origin: TerminalStyleOrigin,
    pub attrs: TerminalAttrs,
    /// Rare text and link metadata is shared so ordinary cells carry only one optional pointer.
    pub extra: Option<Arc<TerminalCellExtra>>,
    pub cursor: bool,
}

/// Heap-backed metadata omitted from the common single-codepoint, non-hyperlink cell.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct TerminalCellExtra {
    zerowidth: String,
    hyperlink: Option<String>,
}

impl TerminalCell {
    pub fn zerowidth(&self) -> &str {
        self.extra
            .as_deref()
            .map_or("", |extra| extra.zerowidth.as_str())
    }

    pub fn hyperlink(&self) -> Option<&str> {
        self.extra
            .as_deref()
            .and_then(|extra| extra.hyperlink.as_deref())
    }

    /// Replaces both rare fields at once while preserving the allocation-free empty state.
    pub fn set_extra(&mut self, zerowidth: String, hyperlink: Option<String>) {
        self.extra = if zerowidth.is_empty() && hyperlink.is_none() {
            None
        } else {
            Some(Arc::new(TerminalCellExtra {
                zerowidth,
                hyperlink,
            }))
        };
    }

    pub fn set_zerowidth(&mut self, zerowidth: String) {
        if zerowidth.is_empty() && self.hyperlink().is_none() {
            self.extra = None;
            return;
        }

        let extra = self
            .extra
            .get_or_insert_with(|| Arc::new(TerminalCellExtra::default()));
        Arc::make_mut(extra).zerowidth = zerowidth;
    }

    pub fn set_hyperlink(&mut self, hyperlink: Option<String>) {
        if hyperlink.is_none() && self.zerowidth().is_empty() {
            self.extra = None;
            return;
        }

        let extra = self
            .extra
            .get_or_insert_with(|| Arc::new(TerminalCellExtra::default()));
        Arc::make_mut(extra).hyperlink = hyperlink;
    }
}

/// Tracks explicit terminal colors in one byte so every snapshot cell stays compact.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct TerminalStyleOrigin(u8);

impl TerminalStyleOrigin {
    const FOREGROUND_EXPLICIT: u8 = 1 << 0;
    const BACKGROUND_EXPLICIT: u8 = 1 << 1;

    pub const fn new(foreground_explicit: bool, background_explicit: bool) -> Self {
        Self(
            (if foreground_explicit {
                Self::FOREGROUND_EXPLICIT
            } else {
                0
            }) | (if background_explicit {
                Self::BACKGROUND_EXPLICIT
            } else {
                0
            }),
        )
    }

    #[inline]
    pub const fn foreground_explicit(self) -> bool {
        self.0 & Self::FOREGROUND_EXPLICIT != 0
    }

    #[inline]
    pub const fn background_explicit(self) -> bool {
        self.0 & Self::BACKGROUND_EXPLICIT != 0
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TerminalColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl TerminalColor {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Stores independent terminal text attributes as bits in snapshot hot storage.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct TerminalAttrs(u8);

impl TerminalAttrs {
    const BOLD: u8 = 1 << 0;
    const DIM: u8 = 1 << 1;
    const ITALIC: u8 = 1 << 2;
    const UNDERLINE: u8 = 1 << 3;
    const STRIKEOUT: u8 = 1 << 4;
    const INVERSE: u8 = 1 << 5;

    pub const fn new(
        bold: bool,
        dim: bool,
        italic: bool,
        underline: bool,
        strikeout: bool,
        inverse: bool,
    ) -> Self {
        Self(
            (if bold { Self::BOLD } else { 0 })
                | (if dim { Self::DIM } else { 0 })
                | (if italic { Self::ITALIC } else { 0 })
                | (if underline { Self::UNDERLINE } else { 0 })
                | (if strikeout { Self::STRIKEOUT } else { 0 })
                | (if inverse { Self::INVERSE } else { 0 }),
        )
    }

    #[inline]
    pub const fn bold(self) -> bool {
        self.0 & Self::BOLD != 0
    }

    #[inline]
    pub const fn dim(self) -> bool {
        self.0 & Self::DIM != 0
    }

    #[inline]
    pub const fn italic(self) -> bool {
        self.0 & Self::ITALIC != 0
    }

    #[inline]
    pub const fn underline(self) -> bool {
        self.0 & Self::UNDERLINE != 0
    }

    #[inline]
    pub const fn strikeout(self) -> bool {
        self.0 & Self::STRIKEOUT != 0
    }

    #[inline]
    pub const fn inverse(self) -> bool {
        self.0 & Self::INVERSE != 0
    }
}

#[derive(Clone, Debug)]
pub struct TerminalRow {
    /// Stable presentation identity assigned by the pane. Zero means unassigned.
    pub line_id: u64,
    /// Opaque identity of the backing emulator row used for adjacent-snapshot reuse.
    pub source_id: usize,
    pub absolute_line: i64,
    pub cells: Arc<Vec<TerminalCell>>,
    pub wrapped: bool,
    pub active_input: bool,
    pub signature: u64,
}

impl TerminalRow {
    pub fn text(&self) -> String {
        let mut text = String::new();
        for cell in self.cells.iter() {
            text.push(cell.ch);
            text.push_str(cell.zerowidth());
        }
        text
    }

    pub fn cells_mut(&mut self) -> &mut Vec<TerminalCell> {
        // Snapshot rows can share unchanged cell buffers across frames. Writers
        // use copy-on-write so older snapshots remain stable.
        Arc::make_mut(&mut self.cells)
    }

    pub fn refresh_signature(&mut self) {
        self.signature = self.compute_signature();
    }

    pub fn compute_signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.wrapped.hash(&mut hasher);
        self.active_input.hash(&mut hasher);
        self.cells.hash(&mut hasher);
        hasher.finish()
    }

    pub fn reuse_cells_from_if_equal(&mut self, previous: &Self) -> bool {
        let same_line = if self.line_id != 0 && previous.line_id != 0 {
            self.line_id == previous.line_id
        } else {
            self.absolute_line == previous.absolute_line
        };
        if self.signature != previous.signature
            || !same_line
            || self.wrapped != previous.wrapped
            || self.active_input != previous.active_input
            || self.cells.as_ref() != previous.cells.as_ref()
        {
            return false;
        }

        self.cells = previous.cells.clone();
        true
    }
}
