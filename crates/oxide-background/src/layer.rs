use crate::{BackgroundFit, BackgroundPreferences};
use gpui::{
    AnyElement, Bounds, DevicePixels, ObjectFit, Pixels, RenderImage, Size, StyledImage, div,
    prelude::*,
};
use std::sync::Arc;

pub fn background_object_fit(fit: BackgroundFit) -> ObjectFit {
    match fit {
        BackgroundFit::Cover => ObjectFit::Cover,
        BackgroundFit::Contain => ObjectFit::Contain,
        BackgroundFit::Fill => ObjectFit::Fill,
        BackgroundFit::Tile => ObjectFit::None,
    }
}

pub fn background_image_layer(
    background: BackgroundPreferences,
    image: Option<Arc<RenderImage>>,
) -> AnyElement {
    let image = if let Some(image) = image {
        let id = ("background-image", image.id.0);
        gpui::img(image).id(id)
    } else if background.fit == BackgroundFit::Tile && background.blur <= 0.01 {
        gpui::img(background.path.clone())
            .with_fallback(|| div().size_full().into_any_element())
            .id(Arc::<std::path::Path>::from(background.path))
    } else {
        return div().absolute().inset_0().into_any_element();
    };
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .child(
            image
                .size_full()
                .object_fit(background_object_fit(background.fit))
                .object_position(background.alignment)
                .opacity(background.opacity.clamp(0.0, 1.0)),
        )
        .into_any_element()
}

pub(crate) fn aligned_background_bounds(
    bounds: Bounds<Pixels>,
    image_size: Size<DevicePixels>,
    fit: BackgroundFit,
    alignment: (f32, f32),
) -> Bounds<Pixels> {
    background_object_fit(fit).get_aligned_bounds(bounds, image_size, alignment)
}
