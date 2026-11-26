use super::{MotionDuration, spring_config};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ParentElement, Pixels, Styled, Window, div, prelude::*, px,
};
use oxide_theme::ThemeTokens;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct HeightState {
    velocity: f32,
    target: f32,
    current: f32,
    updated_at: Option<std::time::Instant>,
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
        let config = spring_config(&self.tokens, MotionDuration::Micro);
        if let Some(updated_at) = current.updated_at {
            let sampled = config.step(
                gpui::SpringState {
                    position: current.current,
                    velocity: current.velocity,
                },
                current.target,
                now.saturating_duration_since(updated_at).as_secs_f32(),
            );
            current.current = sampled.position;
            current.velocity = sampled.velocity;
        }
        if let Some(target) = self.target_height {
            current.target = target;
        }
        let sampled = gpui::SpringState {
            position: current.current,
            velocity: current.velocity,
        };
        let running = animated && !config.is_settled(sampled, current.target, 0.1);
        if !running {
            current.current = current.target;
            current.velocity = 0.0;
        }
        current.updated_at = running.then_some(now);
        let height = current.current.max(0.0);
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
                    state.velocity = 0.0;
                    state.target = height;
                    state.current = height;
                    state.updated_at = None;
                } else if (height - state.target).abs() > 0.1 {
                    state.target = height;
                    state.updated_at = Some(cx.background_executor().now());
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
