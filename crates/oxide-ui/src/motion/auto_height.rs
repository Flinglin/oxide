use super::{MotionDuration, duration, ease_out_cubic};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ParentElement, Pixels, Styled, Window, div, prelude::*, px,
};
use oxide_theme::ThemeTokens;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct HeightState {
    from: f32,
    target: f32,
    current: f32,
    started_at: Option<std::time::Instant>,
}

/// Animates the occupied space of a bounded composer region. Removed content
/// is dropped immediately; element state retains only measured dimensions.
pub struct AutoHeight {
    id: ElementId,
    tokens: ThemeTokens,
    content: Option<AnyElement>,
    rendered: Option<AnyElement>,
    target_height: Option<f32>,
    overflow_when_settled: bool,
}

pub fn auto_height(
    tokens: &ThemeTokens,
    id: impl Into<ElementId>,
    content: Option<AnyElement>,
) -> AutoHeight {
    AutoHeight {
        id: id.into(),
        tokens: *tokens,
        content,
        rendered: None,
        target_height: None,
        overflow_when_settled: false,
    }
}

impl AutoHeight {
    /// Explicit panel geometry keeps content mounted while the viewport closes.
    pub fn target_height(mut self, height: f32) -> Self {
        self.target_height = Some(height.max(0.0));
        self
    }

    /// In-flow completion menus may extend outside the settled panel.
    pub fn overflow_when_settled(mut self) -> Self {
        self.overflow_when_settled = true;
        self
    }
}

impl IntoElement for AutoHeight {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for AutoHeight {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let state =
            window.with_element_state(id.unwrap(), |state: Option<Rc<RefCell<HeightState>>>, _| {
                let state = state.unwrap_or_default();
                (state.clone(), state)
            });
        let animated =
            self.tokens.motion.enabled && self.tokens.motion.spatial_enabled && !cx.reduce_motion();
        let now = cx.background_executor().now();
        let mut current = state.borrow_mut();
        let mut running = if let Some(started_at) = current.started_at {
            let progress = (now - started_at).as_secs_f32()
                / duration(&self.tokens, MotionDuration::Micro)
                    .as_secs_f32()
                    .max(f32::EPSILON);
            current.current =
                current.from + (current.target - current.from) * ease_out_cubic(progress.min(1.0));
            if progress >= 1.0 {
                current.started_at = None;
            }
            progress < 1.0
        } else {
            false
        };
        if let Some(target) = self.target_height {
            if !animated {
                current.from = target;
                current.target = target;
                current.current = target;
                current.started_at = None;
                running = false;
            } else if (target - current.target).abs() > 0.1 {
                current.from = current.current;
                current.target = target;
                current.started_at = Some(now);
                running = true;
            }
        }
        let height = current.current;
        drop(current);
        if animated && running {
            window.request_animation_frame();
        }
        let measured = state.clone();
        let target_height = self.target_height;
        let region = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .when(
                !self.overflow_when_settled || running || height <= 0.0,
                |region| region.overflow_hidden(),
            )
            .children(
                self.content
                    .take()
                    .map(|content| div().w_full().flex_none().flex().flex_col().child(content)),
            )
            .on_children_prepainted(move |bounds, window, cx| {
                if target_height.is_some() {
                    return;
                }
                let height = bounds
                    .first()
                    .map_or(0.0, |bounds| f32::from(bounds.size.height));
                let mut state = measured.borrow_mut();
                if !animated {
                    state.from = height;
                    state.target = height;
                    state.current = height;
                    state.started_at = None;
                } else if (height - state.target).abs() > 0.1 {
                    state.from = state.current;
                    state.target = height;
                    state.started_at = Some(cx.background_executor().now());
                    window.request_animation_frame();
                }
            });
        // Keep the child ID path stable while height changes, preserving keyboard focus.
        let mut region = if animated || target_height.is_some() {
            region.h(px(height))
        } else {
            region
        }
        .into_any_element();
        let layout = region.request_layout(window, cx);
        self.rendered = Some(region);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.rendered.as_mut().unwrap().prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.rendered.as_mut().unwrap().paint(window, cx);
    }
}
