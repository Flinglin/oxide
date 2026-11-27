use super::*;
use std::collections::HashSet;

struct Entry {
    image: ImageCacheItem,
    used: u64,
}

pub(crate) struct RecentImageCache {
    entries: HashMap<u64, Entry>,
    visible: HashSet<u64>,
    clock: u64,
    budget: usize,
    windows: Option<HashMap<crate::WindowId, WindowImages>>,
}

#[derive(Default)]
struct WindowImages {
    requested: HashSet<u64>,
    retained: HashSet<u64>,
}

impl RecentImageCache {
    fn new(budget: usize, cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self {
            entries: HashMap::new(),
            visible: HashSet::new(),
            clock: 0,
            budget,
            windows: None,
        });
        cx.observe_release(&entity, |cache, cx| {
            for entry in cache.entries.values() {
                if let Some(Ok(image)) = entry.image.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        entity
    }

    fn begin_frame(&mut self, window: &mut Window, cx: &mut App) {
        self.evict(window, cx);
        self.visible.clear();
    }

    pub(crate) fn shared(cx: &mut App) -> Entity<Self> {
        let cache = Self::new(64 * 1024 * 1024, cx);
        cache.update(cx, |cache, _| cache.windows = Some(HashMap::new()));
        cache
    }

    pub(crate) fn finish_window_frame(
        &mut self,
        painted: &HashSet<crate::ImageId>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let windows = self.windows.as_mut().expect("shared cache");
        let live: HashSet<_> = cx
            .windows()
            .into_iter()
            .map(|window| window.window_id())
            .collect();
        windows.retain(|id, _| live.contains(id));
        let usage = windows
            .entry(window.window_handle().window_id())
            .or_default();
        usage.retained = std::mem::take(&mut usage.requested);
        // Cached paint subtrees do not call load again; their image IDs still pin the source.
        for (&key, entry) in &self.entries {
            if entry.image.contains_image(painted) {
                usage.retained.insert(key);
            }
        }
        self.visible = windows
            .values()
            .flat_map(|usage| usage.retained.iter().chain(&usage.requested))
            .copied()
            .collect();
        self.evict(window, cx);
    }

    fn mark_used(&mut self, key: u64, window: &Window) {
        self.visible.insert(key);
        if let Some(windows) = &mut self.windows {
            windows
                .entry(window.window_handle().window_id())
                .or_default()
                .requested
                .insert(key);
        }
    }

    pub(crate) fn remove_from_all_windows(&mut self, source: &Resource, cx: &mut App) {
        if let Some(entry) = self.entries.remove(&hash(source))
            && let Some(Ok(image)) = entry.image.get()
        {
            cx.drop_image(image, None);
        }
    }

    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        let mut bytes = 0usize;
        let mut cold = Vec::new();
        for (&key, entry) in &self.entries {
            let cost = entry
                .image
                .retained_bytes()
                .saturating_add(entry.image.estimated_render_bytes());
            bytes = bytes.saturating_add(cost);
            if !self.visible.contains(&key) {
                cold.push((
                    entry.used,
                    key,
                    cost,
                    !matches!(entry.image.get(), Some(Ok(_))),
                ));
            }
        }
        cold.sort_unstable();
        for (_, key, size, unfinished) in cold {
            if !unfinished && self.budget != 0 && bytes <= self.budget {
                continue;
            }
            if let Some(entry) = self.entries.remove(&key) {
                if let Some(Ok(image)) = entry.image.get() {
                    cx.drop_image(image, Some(window));
                }
                bytes = bytes.saturating_sub(size);
            }
        }
        // Visible images may exceed the budget; evicting them during layout
        // would continuously reload the same images instead of displaying them.
    }
}

impl ImageCache for RecentImageCache {
    fn load_at_size(
        &mut self,
        resource: &Resource,
        bounds: Option<crate::Size<crate::DevicePixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_frame_at_size(resource, bounds, ImageFrameRequest::default(), window, cx)
    }

    fn load_frame_at_size(
        &mut self,
        resource: &Resource,
        bounds: Option<crate::Size<crate::DevicePixels>>,
        frame: ImageFrameRequest,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        self.clock += 1;
        self.mark_used(key, window);
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            image: ImageCacheItem::new_sized(resource, cx),
            used: 0,
        });
        entry.used = self.clock;
        entry.image.use_frame_at_size(bounds, frame, window, cx)
    }
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        self.clock += 1;
        self.mark_used(key, window);
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            image: ImageCacheItem::new(resource, cx),
            used: 0,
        });
        entry.used = self.clock;
        entry.image.use_at_size(
            Some(crate::size(i32::MAX.into(), i32::MAX.into())),
            window,
            cx,
        )
    }
}

/// Budget retained CPU storage plus one decoded texture per image, pinning visible images.
/// A zero budget retains only visible entries, including while their loads are pending.
pub fn retain_recent(id: impl Into<ElementId>, bytes: usize) -> impl ImageCacheProvider {
    RecentProvider {
        id: id.into(),
        bytes,
        prepared: false,
    }
}

struct RecentProvider {
    id: ElementId,
    bytes: usize,
    prepared: bool,
}

impl ImageCacheProvider for RecentProvider {
    fn provide(&mut self, window: &mut Window, cx: &mut App) -> AnyImageCache {
        let entity = window.with_global_id(self.id.clone(), |id, window| {
            window.with_element_state::<Entity<RecentImageCache>, _>(id, |cached, _| {
                let entity = cached.unwrap_or_else(|| RecentImageCache::new(self.bytes, cx));
                (entity.clone(), entity)
            })
        });
        if !self.prepared {
            entity.update(cx, |cache, cx| {
                cache.budget = self.bytes;
                cache.begin_frame(window, cx);
            });
            self.prepared = true;
        }
        entity.into()
    }
}
