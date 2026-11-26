use std::{cell::Cell, rc::Rc};

use gpui::{AnimationExt, AnyElement, IntoElement};
use oxide_theme::ThemeTokens;

use super::{MotionDuration, spatial_spring};

/// Window-owned sidebar position, shared by the clipping viewport and resize edge.
/// Retargeting starts at the last drawn width, not at the previous endpoint.
pub struct SidebarMotion {
    from: f32,
    target: f32,
    current: Rc<Cell<f32>>,
    generation: usize,
}

impl SidebarMotion {
    pub fn new(width: f32) -> Self {
        Self {
            from: width,
            target: width,
            current: Rc::new(Cell::new(width)),
            generation: 0,
        }
    }

    pub fn retarget(&mut self, width: f32) {
        if self.target == width {
            return;
        }
        self.from = self.current.get();
        self.target = width;
    }

    /// Manual resizing owns the width immediately and invalidates the old timeline.
    pub fn settle(&mut self, width: f32) {
        self.from = width;
        self.target = width;
        self.current.set(width);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn animate<E: IntoElement + 'static>(
        &self,
        tokens: &ThemeTokens,
        id: &'static str,
        element: E,
        apply_width: impl Fn(E, f32) -> E + 'static,
    ) -> AnyElement {
        let current = self.current.clone();
        element
            .with_spring(
                (id, self.generation),
                spatial_spring(tokens, MotionDuration::Control, self.target)
                    .from(self.from)
                    .with_epsilon(0.1),
                move |element, width| {
                    current.set(width);
                    apply_width(element, width)
                },
            )
            .into_any_element()
    }
}
