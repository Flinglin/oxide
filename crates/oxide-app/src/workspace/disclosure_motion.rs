use super::*;
use std::{
    cell::Cell,
    collections::BTreeMap,
    hash::{Hash, Hasher},
    rc::Rc,
};

#[derive(Default)]
pub(super) struct DisclosureMotions {
    generation: u64,
    entries: BTreeMap<String, DisclosureTransition>,
}

struct DisclosureTransition {
    generation: u64,
    expanded: bool,
    from: f32,
    opacity: Rc<Cell<f32>>,
}

impl DisclosureMotions {
    fn begin(&mut self, key: String, expanded: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        let from = self
            .entries
            .get(&key)
            .map_or(if expanded { 0.0 } else { 1.0 }, |entry| {
                entry.opacity.get()
            });
        self.entries.insert(
            key,
            DisclosureTransition {
                generation: self.generation,
                expanded,
                from,
                opacity: Rc::new(Cell::new(from)),
            },
        );
        self.generation
    }

    fn finish(&mut self, key: &str, generation: u64) -> bool {
        if self
            .entries
            .get(key)
            .is_some_and(|entry| entry.generation == generation)
        {
            self.entries.remove(key);
            true
        } else {
            false
        }
    }

    pub(super) fn retained(&self, key: &str, expanded: bool) -> bool {
        expanded || self.entries.get(key).is_some_and(|entry| !entry.expanded)
    }

    pub(super) fn signature(&self, owner_prefix: &str) -> u64 {
        if self.entries.is_empty() {
            return 0;
        }
        let mut matched = false;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for (key, entry) in self.entries.range(owner_prefix.to_owned()..) {
            if !key.starts_with(owner_prefix) {
                break;
            }
            matched = true;
            key.hash(&mut hash);
            entry.generation.hash(&mut hash);
        }
        if matched { hash.finish() } else { 0 }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn render(
        &self,
        key: &str,
        tokens: &ThemeTokens,
        content: gpui::Div,
        height: Option<f32>,
    ) -> AnyElement {
        let Some(entry) = self.entries.get(key) else {
            return content.into_any_element();
        };
        let from = entry.from;
        let target = if entry.expanded { 1.0 } else { 0.0 };
        let opacity = entry.opacity.clone();
        let height = height.filter(|_| tokens.motion.spatial_enabled);
        content
            .with_animation(
                (
                    gpui::SharedString::from(key.to_owned()),
                    entry.generation as usize,
                ),
                Animation::new(oxide_ui::motion::duration(
                    tokens,
                    oxide_ui::motion::MotionDuration::Micro,
                ))
                .with_easing(oxide_ui::motion::ease_out_cubic),
                move |content, progress| {
                    let value = from + (target - from) * progress;
                    opacity.set(value);
                    content
                        .opacity(value)
                        .when_some(height, |content, height| content.h(px(height * value)))
                },
            )
            .into_any_element()
    }
}

impl WorkspaceApp {
    pub(super) fn begin_disclosure_motion(
        &mut self,
        key: String,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.theme_tokens.motion.enabled {
            self.disclosure_motions.entries.remove(&key);
            return;
        }
        let generation = self.disclosure_motions.begin(key.clone(), expanded);
        let delay =
            oxide_ui::motion::duration(&self.theme_tokens, oxide_ui::motion::MotionDuration::Micro);
        // Only IDs and scalar motion state survive the transition, never message content.
        // The weak workspace handle prevents completion from retaining a closed window.
        cx.spawn(async move |weak, cx| {
            Timer::after(delay).await;
            let _ = weak.update(cx, |this, cx| {
                if this.disclosure_motions.finish(&key, generation) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
