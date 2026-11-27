use super::*;
use crate::{DevicePixels, Size, SvgRenderer, asset_cache::CachedLoad, size};
use std::collections::VecDeque;
use std::rc::Rc;

pub(super) struct SourceInfo {
    source: Resource,
    bytes: Option<Arc<[u8]>>,
    info: Option<super::decoded_size::ImageInfo>,
    layout: Arc<RenderImage>,
    animated: bool,
}

impl SourceInfo {
    pub(super) fn new(source: Resource, bytes: Option<Vec<u8>>) -> Result<Self, ImageCacheError> {
        let (data, declared) = match &source {
            Resource::Image(image) => (image.bytes.as_slice(), Some(image.format)),
            _ => (bytes.as_deref().expect("resource bytes loaded"), None),
        };
        let format = match declared {
            Some(crate::ImageFormat::Png) => Some(image::ImageFormat::Png),
            Some(crate::ImageFormat::Jpeg) => Some(image::ImageFormat::Jpeg),
            Some(crate::ImageFormat::Gif) => Some(image::ImageFormat::Gif),
            Some(crate::ImageFormat::Webp) => Some(image::ImageFormat::WebP),
            _ => None,
        };
        let info = if declared.is_some() && format.is_none() {
            None
        } else {
            super::decoded_size::ImageInfo::read(data, format)?
        };
        let animated = info.is_some_and(|info| info.format == image::ImageFormat::Gif)
            || (info.is_some_and(|info| info.format == image::ImageFormat::WebP)
                && declared.is_none()
                && image::codecs::webp::WebPDecoder::new(Cursor::new(data))?.has_animation());
        let mut result = Self {
            source,
            bytes: bytes.map(Arc::from),
            info,
            animated,
            layout: Arc::new(RenderImage::new(SmallVec::<[image::Frame; 1]>::new())),
        };
        if let Some(info) = info {
            let dimensions = result.rotated_size(info.size);
            result.layout = Arc::new(
                RenderImage::new(SmallVec::<[image::Frame; 1]>::new())
                    .with_intrinsic_size(size(dimensions.0.into(), dimensions.1.into())),
            );
        }
        Ok(result)
    }

    fn quarter_turns(&self) -> u8 {
        match self.source {
            Resource::RotatedImage { quarter_turns, .. } => quarter_turns % 4,
            _ => 0,
        }
    }

    fn rotated_size(&self, size: (u32, u32)) -> (u32, u32) {
        if self.quarter_turns() % 2 == 1 {
            (size.1, size.0)
        } else {
            size
        }
    }

    fn target(&self, bounds: (u32, u32)) -> (u32, u32) {
        self.rotated_size(self.info.unwrap().target(self.rotated_size(bounds)))
    }

    fn bytes(&self) -> &[u8] {
        match &self.source {
            Resource::Image(image) => &image.bytes,
            _ => self.bytes.as_deref().expect("resource bytes loaded"),
        }
    }

    pub(super) fn decode(
        &self,
        target: (u32, u32),
        renderer: SvgRenderer,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        let turns = self.quarter_turns();
        if let Some(info) = self.info {
            let target = info.target(self.rotated_size(target));
            let dimensions = self.rotated_size(info.size);
            let intrinsic = size(dimensions.0.into(), dimensions.1.into());
            let input = match &self.source {
                Resource::Image(image) => super::AnimationInput::Image(image.clone()),
                _ => super::AnimationInput::Bytes(self.bytes.as_ref().unwrap().clone()),
            };
            if let Some(frames) = super::decoded_size::animation(
                info,
                input,
                target,
                turns,
                !matches!(self.source, Resource::Image(_)),
            )? {
                return Ok(Arc::new(frames.with_intrinsic_size(intrinsic)));
            }
            if let Some(pixels) = info.decode(self.bytes(), target)? {
                let mut pixels = super::decoded_size::rotate(pixels, turns);
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                return Ok(Arc::new(
                    RenderImage::from_decoded_frames([image::Frame::new(pixels)])?
                        .with_intrinsic_size(intrinsic),
                ));
            }
        }
        if turns != 0
            && let Ok(format) = image::guess_format(self.bytes())
        {
            // At native zoom (or for unsupported thumbnails), rotate decoded frames
            // before compact storage, without an intermediate full RenderImage copy.
            let frames = decode_static_image(self.bytes(), format)?
                .into_iter()
                .map(|frame| {
                    let delay = frame.delay();
                    image::Frame::from_parts(
                        super::decoded_size::rotate(frame.into_buffer(), turns),
                        0,
                        0,
                        delay,
                    )
                });
            return Ok(Arc::new(RenderImage::from_decoded_frames(frames)?));
        }
        match &self.source {
            Resource::Image(image) => image.to_image_data(renderer).map_err(Into::into),
            _ => ImageAssetLoader::decode_bytes(self.bytes(), renderer),
        }
    }
}

struct Ready {
    target: (u32, u32),
    image: Arc<RenderImage>,
}

/// The cache entry owns encoded input, one worker and at most two display resolutions.
pub(crate) struct SizedImage {
    source: Rc<CachedLoad<Result<Arc<SourceInfo>, ImageCacheError>>>,
    ready: VecDeque<Ready>,
    pending: Option<(
        (u32, u32),
        CachedLoad<Result<Arc<RenderImage>, ImageCacheError>>,
    )>,
    failed: Option<((u32, u32), ImageCacheError)>,
    pending_frame: Option<CachedLoad<Result<(), ImageCacheError>>>,
}

impl SizedImage {
    pub(crate) fn is_animation(&self, window: &Window) -> Option<bool> {
        self.source
            .use_by(window.current_view())
            .map(|result| result.is_ok_and(|source| source.animated))
    }

    pub(crate) fn fork_player(&self) -> Self {
        Self {
            source: self.source.clone(),
            ready: VecDeque::new(),
            pending: None,
            failed: None,
            pending_frame: None,
        }
    }

    pub(crate) fn image_bytes(&self) -> usize {
        self.ready
            .iter()
            .map(|ready| ready.image.retained_bytes())
            .sum()
    }
    pub(crate) fn contains_image(
        &self,
        painted: &std::collections::HashSet<crate::ImageId>,
    ) -> bool {
        self.ready
            .iter()
            .any(|ready| painted.contains(&ready.image.id))
    }
    pub(crate) fn new(source: Resource, cx: &App) -> Self {
        let read = ImageAssetLoader::read_bytes(source.clone(), cx);
        let load = async move {
            let bytes = match &source {
                Resource::Image(_) => None,
                _ => Some(read.await?),
            };
            SourceInfo::new(source, bytes).map(Arc::new)
        };
        Self {
            source: Rc::new(CachedLoad::new(load, cx)),
            ready: VecDeque::new(),
            pending: None,
            failed: None,
            pending_frame: None,
        }
    }

    pub(crate) fn get(&self) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.ready.back().map(|ready| Ok(ready.image.clone()))
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        let input = self
            .source
            .get()
            .and_then(Result::ok)
            .map_or(0, |source| source.bytes().len());
        input + self.image_bytes()
    }

    pub(crate) fn use_image(
        &mut self,
        bounds: Option<Size<DevicePixels>>,
        frame: usize,
        window: &Window,
        cx: &App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let source = match self.source.use_by(window.current_view())? {
            Ok(source) => source,
            Err(error) => return Some(Err(error)),
        };
        if let Some(pending) = &self.pending_frame
            && let Some(result) = pending.use_by(window.current_view())
        {
            self.pending_frame = None;
            if let Err(error) = result {
                return Some(Err(error));
            }
        }
        if let Some((target, pending)) = &self.pending
            && let Some(result) = pending.use_by(window.current_view())
        {
            let target = *target;
            self.pending = None;
            match result {
                Ok(image) => {
                    self.ready.push_back(Ready { target, image });
                    if self.ready.len() > 2 {
                        self.ready.pop_front();
                    }
                    self.failed = None;
                }
                Err(error) => self.failed = Some((target, error)),
            }
        }
        let target = if source.info.is_some() {
            let Some(bounds) = bounds else {
                if let Some((_, error)) = &self.failed {
                    return Some(Err(error.clone()));
                }
                let image = self
                    .ready
                    .back()
                    .map_or_else(|| source.layout.clone(), |ready| ready.image.clone());
                return self.display_frame(image, frame, window, cx);
            };
            // Bucket resize steps upward, never below the actual device-pixel request.
            source.target((
                (bounds.width.0.max(1) as u32)
                    .div_ceil(64)
                    .saturating_mul(64),
                (bounds.height.0.max(1) as u32)
                    .div_ceil(64)
                    .saturating_mul(64),
            ))
        } else {
            // Other formats keep the established full-resolution decoder.
            (u32::MAX, u32::MAX)
        };
        if let Some((failed, error)) = &self.failed
            && *failed == target
        {
            return Some(Err(error.clone()));
        }
        let adequate = self
            .ready
            .iter()
            .filter(|ready| ready.target.0 >= target.0 && ready.target.1 >= target.1)
            .min_by_key(|ready| ready.target.0 as u64 * ready.target.1 as u64);
        if let Some(ready) = adequate
            && ready.target.0 <= target.0.saturating_mul(2)
            && ready.target.1 <= target.1.saturating_mul(2)
        {
            return self.display_frame(ready.image.clone(), frame, window, cx);
        }
        let previous = adequate
            .or_else(|| self.ready.back())
            .map(|ready| ready.image.clone());
        if self.pending.is_none() {
            let renderer = cx.svg_renderer();
            let pending = CachedLoad::new(async move { source.decode(target, renderer) }, cx);
            pending.use_by(window.current_view());
            self.pending = Some((target, pending));
        }
        previous.and_then(|image| self.display_frame(image, frame, window, cx))
    }

    fn display_frame(
        &mut self,
        image: Arc<RenderImage>,
        frame: usize,
        window: &Window,
        cx: &App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let frame = frame.min(image.frame_count().saturating_sub(1));
        if image.frame_ready(frame) {
            image.prefetch_frame(frame + 1);
            if image.frame_count() > 1
                && self
                    .ready
                    .back()
                    .is_some_and(|ready| ready.image.id == image.id)
            {
                self.ready.retain(|ready| ready.image.id == image.id);
            }
            return Some(Ok(image));
        }
        if self.pending_frame.is_none() {
            let pending = CachedLoad::new(async move { image.prepare_frame(frame).await }, cx);
            pending.use_by(window.current_view());
            self.pending_frame = Some(pending);
        }
        self.ready
            .iter()
            .rev()
            .find(|ready| ready.image.frame_ready(frame))
            .map(|ready| Ok(ready.image.clone()))
    }
}
