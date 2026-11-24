use crate::{HighlightSpan, PluginGrammar, SyntaxEdit, SyntaxError, SyntaxSession, SyntaxWork};
use oxide_editor_core::{BufferOffset, TextRange};
use std::{cell::Cell, ops::ControlFlow, sync::Arc};
use tree_sitter::{Query, QueryCursor, StreamingIterator};

pub(crate) struct PluginInjection {
    pub query: Query,
    pub grammar: Arc<PluginGrammar>,
}

pub(crate) struct InjectionSyntax {
    definition: usize,
    pub syntax: Box<SyntaxSession>,
    ranges: Vec<tree_sitter::Range>,
}

impl SyntaxSession {
    pub(crate) fn refresh_injections(
        &mut self,
        source: &str,
        edit: Option<SyntaxEdit>,
        work: Option<&SyntaxWork>,
    ) -> Result<(), SyntaxError> {
        let mut previous = std::mem::take(&mut self.injections);
        for (index, definition) in self.queries.injections.iter().enumerate() {
            // One parser per embedded language keeps global byte coordinates and
            // avoids a separate WASM execution store for every template expression.
            let ranges = injection_ranges(&definition.query, &self.tree, source, work)?;
            if ranges.is_empty() {
                continue;
            }
            let syntax =
                if let Some(position) = previous.iter().position(|item| item.definition == index) {
                    let mut syntax = previous.remove(position).syntax;
                    syntax
                        .parser
                        .set_included_ranges(&ranges)
                        .map_err(|error| SyntaxError::Plugin(error.to_string()))?;
                    if let Some(edit) = edit {
                        syntax.apply_edit_controlled(source, edit, work)?;
                    } else {
                        syntax.reparse_controlled(source, work)?;
                    }
                    syntax
                } else {
                    Box::new(Self::parse_plugin_in_ranges(
                        &definition.grammar,
                        source,
                        &ranges,
                        work,
                    )?)
                };
            self.injections.push(InjectionSyntax {
                definition: index,
                syntax,
                ranges,
            });
        }
        Ok(())
    }
}

impl InjectionSyntax {
    pub(crate) fn overlay_highlights(
        &self,
        source: &str,
        range: TextRange,
        spans: Vec<HighlightSpan>,
        work: Option<&SyntaxWork>,
    ) -> Result<Vec<HighlightSpan>, SyntaxError> {
        let mut combined = Vec::with_capacity(spans.len());
        for span in spans {
            crate::work::checkpoint(work)?;
            let mut start = span.range.start.0;
            let end = span.range.end.0;
            for included in self.overlapping_ranges(start, end) {
                if start < included.start_byte {
                    combined.push(HighlightSpan {
                        range: TextRange::new(
                            BufferOffset(start),
                            BufferOffset(included.start_byte.min(end)),
                        ),
                        scope: span.scope,
                    });
                }
                start = start.max(included.end_byte).min(end);
            }
            if start < end {
                combined.push(HighlightSpan {
                    range: TextRange::new(BufferOffset(start), BufferOffset(end)),
                    scope: span.scope,
                });
            }
        }
        // Embedded captures own only their included regions. Parent captures
        // keep delimiters; a child node spanning separate regions cannot color tags.
        for span in self.syntax.highlights_controlled(source, range, work)? {
            crate::work::checkpoint(work)?;
            for included in self.overlapping_ranges(span.range.start.0, span.range.end.0) {
                combined.push(HighlightSpan {
                    range: TextRange::new(
                        BufferOffset(span.range.start.0.max(included.start_byte)),
                        BufferOffset(span.range.end.0.min(included.end_byte)),
                    ),
                    scope: span.scope,
                });
            }
        }
        Ok(combined)
    }

    fn overlapping_ranges(
        &self,
        start: usize,
        end: usize,
    ) -> impl Iterator<Item = &tree_sitter::Range> {
        let first = self.ranges.partition_point(|range| range.end_byte <= start);
        self.ranges[first..]
            .iter()
            .take_while(move |range| range.start_byte < end)
    }
}

fn injection_ranges(
    query: &Query,
    tree: &tree_sitter::Tree,
    source: &str,
    work: Option<&SyntaxWork>,
) -> Result<Vec<tree_sitter::Range>, SyntaxError> {
    let mut cursor = QueryCursor::new();
    let paused = Cell::new(false);
    let mut pause = |_: &tree_sitter::QueryCursorState| {
        let stop = work.is_some_and(SyntaxWork::should_pause);
        paused.set(stop);
        if stop {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut captures = cursor.captures_with_options(
        query,
        tree.root_node(),
        source.as_bytes(),
        tree_sitter::QueryCursorOptions::new().progress_callback(&mut pause),
    );
    let mut ranges = Vec::new();
    loop {
        crate::work::checkpoint(work)?;
        captures.advance();
        let Some((query_match, capture_index)) = captures.get() else {
            if paused.replace(false) {
                continue;
            }
            break;
        };
        let capture = query_match.captures()[*capture_index];
        if query.capture_names()[capture.index as usize] == "injection.content"
            && !capture.node.byte_range().is_empty()
        {
            ranges.push(capture.node.range());
        }
    }
    ranges.sort_by_key(|range| (range.start_byte, range.end_byte));
    let mut merged: Vec<tree_sitter::Range> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start_byte <= previous.end_byte
        {
            if range.end_byte > previous.end_byte {
                previous.end_byte = range.end_byte;
                previous.end_point = range.end_point;
            }
        } else {
            merged.push(range);
        }
    }
    Ok(merged)
}
