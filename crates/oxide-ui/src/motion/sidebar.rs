use std::{cell::Cell, rc::Rc};

use gpui::{Animation, AnimationExt, AnyElement, IntoElement};
use oxide_theme::ThemeTokens;

use super::{MotionDuration, duration};

// CSS cubic-bezier(0.25, 1, 0.5, 1). Its x derivative is at least 0.75,
// so bounded Newton iterations converge without a general curve solver.
fn sidebar_ease_out(progress: f32) -> f32 {
    let x = progress.clamp(0.0, 1.0);
    let mut t = x;
    for _ in 0..5 {
        t -= (0.75 * t + 0.25 * t * t * t - x) / (0.75 + 0.75 * t * t);
    }
    1.0 - (1.0 - t).powi(3)
}

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
        self.generation = self.generation.wrapping_add(1);
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
        if !tokens.motion.enabled || !tokens.motion.spatial_enabled || self.from == self.target {
            self.current.set(self.target);
            return apply_width(element, self.target).into_any_element();
        }
        let from = self.from;
        let target = self.target;
        let current = self.current.clone();
        element
            .with_animation(
                (id, self.generation),
                Animation::new(duration(tokens, MotionDuration::Control))
                    .with_easing(sidebar_ease_out),
                move |element, progress| {
                    let width = from + (target - from) * progress;
                    current.set(width);
                    apply_width(element, width)
                },
            )
            .into_any_element()
    }
}
