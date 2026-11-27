use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{App, ScrollHandle, Window, point, px};

use crate::{
    layout::MarkdownBlockLayout,
    model::{Block, Inline, MarkdownDocument},
    options::MarkdownOptions,
};

#[derive(Clone, Debug)]
pub struct MarkdownNavigation {
    scroll: ScrollHandle,
    state: Rc<RefCell<NavigationState>>,
}

#[derive(Default, Debug)]
struct NavigationState {
    offsets: HashMap<String, f32>,
    pending: Option<String>,
    ancestors: HashMap<String, Vec<String>>,
    returns: HashMap<String, String>,
    disclosures: crate::disclosure::DisclosureState,
}

impl MarkdownNavigation {
    /// Drop derived target maps while preserving disclosure and return-link state.
    pub fn release_targets(&self) {
        let mut state = self.state.borrow_mut();
        state.offsets = HashMap::new();
        state.ancestors = HashMap::new();
    }

    pub fn new(scroll: ScrollHandle) -> Self {
        Self {
            scroll,
            state: Default::default(),
        }
    }

    pub(crate) fn prepare(&self, document: &MarkdownDocument, opts: &MarkdownOptions) {
        let layout = MarkdownBlockLayout::from_document(document, opts);
        let mut state = self.state.borrow_mut();
        state.disclosures = opts.disclosures.clone();
        state.ancestors.clear();
        let mut top = 0.0;
        for (block, size) in document.blocks.iter().zip(layout.item_sizes().iter()) {
            register_targets(block, top, &[], &mut state);
            top += f32::from(size.height) + opts.block_gap;
        }
        for (index, footnote) in document.footnotes.iter().enumerate() {
            register_target(format!("fn:{}", index + 1), top, &[], &mut state);
            for block in &footnote.blocks {
                register_targets(block, top, &[], &mut state);
            }
        }
    }

    pub(crate) fn open(&self, fragment: &str, window: &mut Window) {
        let Some(encoded) = fragment.strip_prefix('#') else {
            return;
        };
        let Some(id) = decode_fragment(encoded) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        if let Some(top) = state.select_target(&id) {
            self.scroll.set_offset(point(px(0.0), px(-top)));
            window.refresh();
        }
    }

    pub(crate) fn measure(&self, id: &str, y: f32, window: &mut Window, cx: &mut App) {
        let top = y - f32::from(self.scroll.bounds().origin.y) - f32::from(self.scroll.offset().y);
        let mut state = self.state.borrow_mut();
        state.offsets.insert(id.to_string(), top);
        if state.pending.as_deref() == Some(id) {
            state.pending = None;
            let scroll = self.scroll.clone();
            window.defer(cx, move |window, _| {
                scroll.set_offset(point(px(0.0), px(-top)));
                window.refresh();
            });
        }
    }
}

impl NavigationState {
    fn select_target(&mut self, id: &str) -> Option<f32> {
        let id = self.destination(id);
        let parents = self.ancestors.get(&id)?;
        let top = *self.offsets.get(&id)?;
        self.disclosures.open(parents);
        self.pending = Some(id);
        Some(top)
    }
    fn destination(&mut self, id: &str) -> String {
        if let Some((target, occurrence)) = id.split_once(":from:")
            && target.starts_with("fn:")
        {
            self.returns.insert(
                target.to_owned(),
                format!("fnref:{}:{occurrence}", &target[3..]),
            );
            target.to_owned()
        } else if let Some(index) = id.strip_prefix("fnback:") {
            self.returns
                .get(&format!("fn:{index}"))
                .cloned()
                .unwrap_or_else(|| format!("fnref:{index}:1"))
        } else {
            id.to_owned()
        }
    }
}

fn register_target(id: String, top: f32, parents: &[String], state: &mut NavigationState) {
    state.offsets.entry(id.clone()).or_insert(top);
    state.ancestors.insert(id, parents.to_vec());
}

fn register_inlines(inlines: &[Inline], top: f32, parents: &[String], state: &mut NavigationState) {
    for inline in inlines {
        match inline {
            Inline::FootnoteReference {
                index, occurrence, ..
            } => register_target(format!("fnref:{index}:{occurrence}"), top, parents, state),
            Inline::Bold(children)
            | Inline::Italic(children)
            | Inline::Strikethrough(children)
            | Inline::Kbd(children)
            | Inline::Subscript(children)
            | Inline::Superscript(children)
            | Inline::Underline(children)
            | Inline::Highlight(children)
            | Inline::Link { text: children, .. } => {
                register_inlines(children, top, parents, state)
            }
            _ => {}
        }
    }
}

fn register_targets(block: &Block, top: f32, parents: &[String], state: &mut NavigationState) {
    match block {
        Block::Located { block, .. } => register_targets(block, top, parents, state),
        Block::Heading { id, inlines, .. } => {
            register_target(id.clone(), top, parents, state);
            register_inlines(inlines, top, parents, state);
        }
        Block::Paragraph { inlines } => register_inlines(inlines, top, parents, state),
        Block::Blockquote { blocks, .. } | Block::HtmlContainer { blocks, .. } => {
            for block in blocks {
                register_targets(block, top, parents, state);
            }
        }
        Block::Details {
            id,
            blocks,
            summary,
            ..
        } => {
            register_inlines(summary, top, parents, state);
            let mut nested = parents.to_vec();
            nested.push(id.clone());
            for block in blocks {
                register_targets(block, top, &nested, state);
            }
        }
        Block::UnorderedList { items } | Block::OrderedList { items, .. } => {
            for item in items {
                register_inlines(&item.inlines, top, parents, state);
                for block in &item.children {
                    register_targets(block, top, parents, state);
                }
            }
        }
        Block::Table { headers, rows, .. } => {
            for cell in headers.iter().chain(rows.iter().flatten()) {
                for block in cell {
                    register_targets(block, top, parents, state);
                }
            }
        }
        _ => {}
    }
}

fn decode_fragment(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut source = value.bytes();
    while let Some(byte) = source.next() {
        bytes.push(if byte == b'%' {
            let high = (source.next()? as char).to_digit(16)?;
            let low = (source.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(bytes).ok()
}
