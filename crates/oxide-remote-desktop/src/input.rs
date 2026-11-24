use std::{cell::RefCell, rc::Rc};

use gpui::{Bounds, Pixels, Point};
use oxide_remote_desktop_core::{
    RemoteDesktopHelperRequest, RemoteDesktopSize, RemoteDesktopWheelDelta,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemoteDesktopMappedPoint {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Default)]
pub struct SharedRemoteDesktopGeometry(Rc<RefCell<RemoteDesktopGeometry>>);

#[derive(Clone, Copy, Debug, Default)]
struct RemoteDesktopGeometry {
    image_bounds: Option<Bounds<Pixels>>,
    frame_size: Option<RemoteDesktopSize>,
    viewport_size: Option<RemoteDesktopSize>,
}

impl SharedRemoteDesktopGeometry {
    pub fn clear(&self) {
        *self.0.borrow_mut() = RemoteDesktopGeometry::default();
    }

    pub(crate) fn update(
        &self,
        image_bounds: Option<Bounds<Pixels>>,
        frame_size: Option<RemoteDesktopSize>,
        viewport_size: Option<RemoteDesktopSize>,
    ) {
        *self.0.borrow_mut() = RemoteDesktopGeometry {
            image_bounds,
            frame_size,
            viewport_size,
        };
    }

    pub fn viewport_size(&self) -> Option<RemoteDesktopSize> {
        self.0.borrow().viewport_size
    }

    pub fn map_window_point(&self, position: Point<Pixels>) -> Option<RemoteDesktopMappedPoint> {
        let geometry = self.0.borrow();
        let bounds = geometry.image_bounds?;
        let remote_size = geometry.frame_size?;
        let viewport_width = f32::from(bounds.size.width).max(1.0);
        let viewport_height = f32::from(bounds.size.height).max(1.0);
        let local_x = f32::from(position.x) - f32::from(bounds.origin.x);
        let local_y = f32::from(position.y) - f32::from(bounds.origin.y);
        if local_x < 0.0 || local_y < 0.0 || local_x > viewport_width || local_y > viewport_height {
            return None;
        }
        RemoteDesktopViewportMapper::new(remote_size, viewport_width, viewport_height)
            .map(|mapper| mapper.map_point(local_x, local_y))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemoteDesktopViewportMapper {
    remote_size: RemoteDesktopSize,
    viewport_width: f32,
    viewport_height: f32,
}

impl RemoteDesktopViewportMapper {
    pub fn new(
        remote_size: RemoteDesktopSize,
        viewport_width: f32,
        viewport_height: f32,
    ) -> Option<Self> {
        if viewport_width <= 0.0 || viewport_height <= 0.0 {
            return None;
        }

        Some(Self {
            remote_size,
            viewport_width,
            viewport_height,
        })
    }

    pub fn map_point(self, local_x: f32, local_y: f32) -> RemoteDesktopMappedPoint {
        let scaled_x = scale_axis(local_x, self.viewport_width, self.remote_size.width);
        let scaled_y = scale_axis(local_y, self.viewport_height, self.remote_size.height);
        RemoteDesktopMappedPoint {
            x: scaled_x,
            y: scaled_y,
        }
    }

    pub fn mouse_move_request(self, local_x: f32, local_y: f32) -> RemoteDesktopHelperRequest {
        let point = self.map_point(local_x, local_y);
        RemoteDesktopHelperRequest::MouseMove {
            x: point.x,
            y: point.y,
        }
    }

    pub fn resize_request(width: f32, height: f32) -> Option<RemoteDesktopHelperRequest> {
        if width <= 0.0 || height <= 0.0 {
            return None;
        }

        Some(RemoteDesktopHelperRequest::Resize {
            size: RemoteDesktopSize::clamped(width.round() as u32, height.round() as u32),
            scale_factor: None,
        })
    }

    pub fn wheel_request(delta_x: f32, delta_y: f32) -> RemoteDesktopHelperRequest {
        RemoteDesktopHelperRequest::Wheel {
            delta: RemoteDesktopWheelDelta {
                x: delta_x,
                y: delta_y,
            },
        }
    }
}

fn scale_axis(local: f32, viewport: f32, remote: u32) -> u32 {
    if remote == 0 {
        return 0;
    }

    let max = remote.saturating_sub(1) as f32;
    let ratio = (local / viewport).clamp(0.0, 1.0);
    (ratio * max).round() as u32
}
