use std::ops::Range;

use oxide_terminal_core::{TerminalAttrs, TerminalCommandMark, TerminalSnapshot};
use oxide_terminal_semantic::{
    CompiledSemanticScheme, SemanticClass, SemanticLineRole, SemanticShellDialect,
    classify_line_with_compiled_scheme_and_shell, semantic_line_emphasis,
    semantic_output_role_for_command,
};

use crate::terminal_ui::{
    TerminalUiTheme, terminal_color_from_hex, terminal_semantic_color, terminal_semantic_line_band,
    terminal_semantic_variant_color,
};
use crate::terminal_view::element::{TerminalRect, to_hsla};
use crate::terminal_view::highlight::{
    TerminalHighlightLayout, build_logical_line, logical_line_range,
};

pub(super) fn append_terminal_semantics_for_rows(
    snapshot: &TerminalSnapshot,
    command_marks: &[TerminalCommandMark],
    rows: Range<usize>,
    theme: &TerminalUiTheme,
    semantic_scheme: &CompiledSemanticScheme,
    semantic_shell: SemanticShellDialect,
    layout: &mut TerminalHighlightLayout,
) {
    let mut seen_lines = std::collections::HashSet::new();
    for row in rows {
        let Some(line_range) = logical_line_range(snapshot, row) else {
            continue;
        };
        if !seen_lines.insert(line_range.clone()) {
            continue;
        }
        let role = semantic_line_role_for_rows(snapshot, command_marks, line_range.clone());
        let line = build_logical_line(snapshot, line_range.clone());
        let spans = classify_line_with_compiled_scheme_and_shell(
            &line.text,
            role,
            semantic_scheme,
            semantic_shell,
        );
        if let Some(emphasis) = semantic_line_emphasis(&line.text, role)
            && spans.iter().any(|span| span.class == emphasis)
        {
            append_semantic_line_band(snapshot, line_range, theme, emphasis, layout);
        }
        for span in spans {
            let start = line.text[..span.range.start].chars().count();
            let end = line.text[..span.range.end].chars().count();
            let Some(cells) = line.map.get(start..end) else {
                continue;
            };
            let Some(span_text) = line.text.get(span.range.clone()) else {
                continue;
            };
            let foreground = semantic_foreground_for_variant(
                theme,
                span.class,
                span.style_variant,
                semantic_scheme,
            );
            let operator_foreground =
                matches!(span.class, SemanticClass::Timestamp | SemanticClass::Option)
                    .then(|| semantic_foreground(theme, SemanticClass::Operator, semantic_scheme));
            let mut option_prefix = span.class == SemanticClass::Option;
            for (ch, mapped) in span_text.chars().zip(cells) {
                let class = semantic_component_class(span.class, ch, option_prefix);
                if option_prefix && ch != '-' {
                    option_prefix = false;
                }
                let key = (mapped.row, mapped.col);
                if layout.foregrounds.contains_key(&key) {
                    continue;
                }
                let Some(cell) = snapshot
                    .lines
                    .get(mapped.row)
                    .and_then(|row| row.cells.get(mapped.col))
                else {
                    continue;
                };
                // Semantic colors fill only genuinely unstyled terminal text.
                if cell.style_origin.foreground_explicit()
                    || cell.style_origin.background_explicit()
                    || cell.attrs != TerminalAttrs::default()
                {
                    continue;
                }
                layout.foregrounds.insert(
                    key,
                    if class == span.class {
                        foreground
                    } else {
                        operator_foreground.unwrap_or(foreground)
                    },
                );
            }
        }
    }
}

fn append_semantic_line_band(
    snapshot: &TerminalSnapshot,
    rows: Range<usize>,
    theme: &TerminalUiTheme,
    class: SemanticClass,
    layout: &mut TerminalHighlightLayout,
) {
    // Bands are intentionally quieter than token colors and remain behind selection/search paint.
    let Some((color, opacity)) = terminal_semantic_line_band(theme, class) else {
        return;
    };
    let existing_background_overlaps = layout
        .backgrounds
        .iter()
        .any(|background| rows.contains(&background.row));
    let has_explicit_background = snapshot.lines.get(rows.clone()).is_none_or(|rows| {
        rows.iter().any(|row| {
            row.cells
                .iter()
                .any(|cell| cell.style_origin.background_explicit())
        })
    });
    if existing_background_overlaps || has_explicit_background {
        return;
    }

    let mut color = to_hsla(terminal_color_from_hex(color));
    color.a = opacity;
    layout.backgrounds.extend(rows.map(|row| TerminalRect {
        row,
        col: 0,
        cells: snapshot.cols,
        color,
    }));
}

fn semantic_component_class(class: SemanticClass, ch: char, option_prefix: bool) -> SemanticClass {
    // Structured punctuation gets its own color without changing the stable
    // semantic scheme format or fragmenting public timestamp/option spans.
    if class == SemanticClass::Timestamp && matches!(ch, ':' | '.' | ',' | '-' | '/' | '+') {
        SemanticClass::Operator
    } else if class == SemanticClass::Option && option_prefix && ch == '-' {
        SemanticClass::Operator
    } else {
        class
    }
}

pub(super) fn semantic_line_role_for_rows(
    snapshot: &TerminalSnapshot,
    command_marks: &[TerminalCommandMark],
    rows: Range<usize>,
) -> SemanticLineRole {
    if snapshot
        .lines
        .get(rows.clone())
        .is_some_and(|lines| lines.iter().any(|row| row.active_input))
    {
        return SemanticLineRole::Command;
    }

    let viewport_start = snapshot
        .scrollback_lines
        .saturating_sub(snapshot.display_offset);
    let start_line = viewport_start.saturating_add(rows.start);
    let end_line = viewport_start.saturating_add(rows.end.saturating_sub(1));
    if command_marks.iter().any(|mark| {
        !mark.command_line_clipped && (start_line..=end_line).contains(&mark.command_line)
    }) {
        return SemanticLineRole::Command;
    }
    if let Some(mark) = command_marks.iter().rev().find(|mark| {
        let output_start = mark.output_start_line();
        let output_end = mark.end_line.unwrap_or(end_line);
        output_start <= end_line && output_end >= start_line
    }) {
        return mark
            .command
            .as_deref()
            .map(semantic_output_role_for_command)
            .unwrap_or(SemanticLineRole::Output);
    }
    SemanticLineRole::Unknown
}

fn semantic_foreground(
    theme: &TerminalUiTheme,
    class: SemanticClass,
    semantic_scheme: &CompiledSemanticScheme,
) -> gpui::Hsla {
    let color = terminal_semantic_color(theme, class, semantic_scheme);
    to_hsla(terminal_color_from_hex(color))
}

fn semantic_foreground_for_variant(
    theme: &TerminalUiTheme,
    class: SemanticClass,
    style_variant: Option<u8>,
    semantic_scheme: &CompiledSemanticScheme,
) -> gpui::Hsla {
    let color = terminal_semantic_variant_color(theme, class, style_variant, semantic_scheme);
    to_hsla(terminal_color_from_hex(color))
}
