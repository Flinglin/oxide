use super::*;

const CROSSFADE_DURATION: Duration = Duration::from_millis(300);

#[derive(Clone)]
enum Picture {
    Image(Arc<RenderImage>),
    Stream(Entity<Player>),
}

#[derive(Clone)]
struct Presented {
    key: SourceKey,
    preferences: BackgroundPreferences,
    picture: Picture,
}

struct Surface {
    background: BackgroundPreferences,
    image: Option<Arc<RenderImage>>,
    key: Option<SourceKey>,
    player: Option<Entity<Player>>,
    consumer: Rc<RefCell<Consumer>>,
    observation: Option<Subscription>,
    displayed: Option<Presented>,
    outgoing: Option<(Presented, Instant)>,
    repaint: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn media_layer(
    background: BackgroundPreferences,
    image: Option<Arc<RenderImage>>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if background.path.as_os_str().is_empty() || background.opacity <= 0.0 {
        return div().absolute().inset_0().into_any_element();
    }
    // One mounted owner survives image/video changes and retains only the visible transition.
    let surface = window
        .use_keyed_state("background-media", cx, |window, cx| {
            cx.new(|cx| Surface {
                background: background.clone(),
                image: None,
                key: None,
                player: None,
                consumer: Rc::default(),
                observation: None,
                displayed: None,
                outgoing: None,
                repaint: None,
                _subscriptions: vec![
                    cx.observe_window_activation(window, |_: &mut Surface, _, cx| cx.notify()),
                    cx.observe_window_visibility(window, |_: &mut Surface, _, _, cx| cx.notify()),
                ],
            })
        })
        .read(cx)
        .clone();
    surface.update(cx, |surface, _| {
        surface.background = background;
        surface.image = image;
    });
    surface.into_any_element()
}

impl Surface {
    fn select_source(&mut self, key: SourceKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.observation = None;
        self.consumer.borrow_mut().visible = false;
        if let Some(previous) = self.player.take() {
            previous.update(cx, |player, _| {
                if !player
                    .consumers
                    .iter()
                    .filter_map(Weak::upgrade)
                    .any(|consumer| consumer.borrow().visible)
                {
                    player.freeze();
                }
            });
        }
        self.consumer = Rc::default();
        if is_streaming_source(&key.path) {
            let player = cx
                .default_global::<Players>()
                .0
                .get(&key)
                .and_then(WeakEntity::upgrade)
                .unwrap_or_else(|| {
                    let player = cx.new(|cx| {
                        Player::new(key.clone(), self.background.on_failure.clone(), window, cx)
                    });
                    let players = &mut cx.default_global::<Players>().0;
                    players.retain(|_, player| player.upgrade().is_some());
                    players.insert(key.clone(), player.downgrade());
                    player
                });
            player.update(cx, |player, _| {
                player.consumers.push(Rc::downgrade(&self.consumer))
            });
            self.observation = Some(cx.observe(&player, |_, _, cx| cx.notify()));
            self.player = Some(player);
        }
        self.key = Some(key);
    }

    fn accept_ready(&mut self, ready: Option<Presented>, now: Instant, reduce_motion: bool) {
        if self.outgoing.as_ref().is_some_and(|(_, start)| {
            reduce_motion || now.saturating_duration_since(*start) >= CROSSFADE_DURATION
        }) {
            self.outgoing = None;
            self.repaint = None;
        }
        let Some(ready) = ready else {
            return;
        };
        if self
            .displayed
            .as_ref()
            .is_some_and(|displayed| displayed.key == ready.key)
        {
            self.displayed = Some(ready);
        } else if self.outgoing.is_none() {
            // Finish a transition already on screen before promoting the latest ready source.
            // Pending selections are replaceable; they never form an unbounded chain of layers.
            self.outgoing = self
                .displayed
                .take()
                .filter(|_| !reduce_motion)
                .map(|old| (old, now));
            self.displayed = Some(ready);
        }
    }

    fn schedule(&mut self, visible: bool, window: &Window, cx: &mut Context<Self>) {
        if self.outgoing.is_none()
            || !visible
            || !window.is_visible()
            || window.is_minimized()
            || !window.is_window_active()
            || cx.reduce_motion()
        {
            self.repaint = None;
        } else if self.repaint.is_none() {
            // The mounted surface owns this short-lived repaint; release cancels it.
            self.repaint = Some(cx.spawn(async move |surface, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let _ = surface.update(cx, |surface, cx| {
                    surface.repaint = None;
                    cx.notify();
                });
            }));
        }
    }
}

impl Render for Surface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let streaming = is_streaming_source(&self.background.path);
        let gpu_processing = streaming
            && window
                .gpu_specs()
                .is_some_and(|specs| !specs.is_software_emulated);
        let key = SourceKey {
            window: window.window_handle().window_id(),
            path: self.background.path.clone(),
            fit: if gpu_processing {
                BackgroundFit::Contain
            } else {
                self.background.fit
            },
            blur: if gpu_processing {
                0
            } else {
                (self.background.blur * 1000.0).round().max(0.0) as u32
            },
            limits: self.background.limits,
            gpu_processing,
        };
        self.select_source(key.clone(), window, cx);
        let picture = if let Some(player) = &self.player {
            player.update(cx, |player, _| {
                player.on_failure = self.background.on_failure.clone()
            });
            player
                .read(cx)
                .current
                .as_ref()
                .map(|_| Picture::Stream(player.clone()))
        } else if self.background.fit == BackgroundFit::Tile && self.background.blur <= 0.01 {
            // Keep native-size image semantics while observing readiness before replacing the old view.
            window
                .use_asset::<gpui::ImgResourceLoader>(
                    &gpui::Resource::from(self.background.path.clone()),
                    cx,
                )
                .and_then(Result::ok)
                .map(Picture::Image)
        } else {
            self.image.clone().map(Picture::Image)
        };
        let now = cx.background_executor().now();
        self.accept_ready(
            picture.map(|picture| Presented {
                key: key.clone(),
                preferences: self.background.clone(),
                picture,
            }),
            now,
            cx.reduce_motion(),
        );
        let progress = self.outgoing.as_ref().map_or(1.0, |(_, start)| {
            let t = (now.saturating_duration_since(*start).as_secs_f32()
                / CROSSFADE_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        });
        let surface = cx.entity().downgrade();
        let mut layer = div().absolute().inset_0().overflow_hidden().child(
            canvas(
                move |bounds, window, cx| {
                    let visible = bounds.intersects(&window.content_mask().bounds);
                    let _ = surface.update(cx, |surface, cx| surface.schedule(visible, window, cx));
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        if let Some(player) = &self.player {
            let player = player.clone();
            let prepare = player.clone();
            let consumer = self.consumer.clone();
            let paint_consumer = consumer.clone();
            let surface = cx.entity().downgrade();
            let selected = self
                .displayed
                .as_ref()
                .is_some_and(|current| current.key == key);
            let paused = self.background.scene.paused || !selected;
            let preferences = self.background.clone();
            // Upload/import the new first frame without showing it before the old picture is ready to fade.
            layer = layer.child(
                div().absolute().inset_0().opacity(0.0).child(
                    canvas(
                        move |bounds, window, cx| {
                            let scale = window.scale_factor();
                            *consumer.borrow_mut() = Consumer {
                                paused,
                                width: (bounds.size.width.as_f32() * scale).ceil().max(1.0) as u32,
                                height: (bounds.size.height.as_f32() * scale).ceil().max(1.0)
                                    as u32,
                                visible: bounds.intersects(&window.content_mask().bounds),
                            };
                            player.update(cx, |player, cx| player.sync(window, cx));
                        },
                        move |bounds, _, window, cx| {
                            if !paint_consumer.borrow().visible || selected {
                                return;
                            }
                            let became_ready = prepare.update(cx, |player, cx| {
                                let ready = player.current.is_some();
                                player.paint(
                                    bounds,
                                    preferences.alignment,
                                    preferences.fit,
                                    preferences.blur,
                                    window,
                                    cx,
                                );
                                !ready && player.current.is_some()
                            });
                            if became_ready {
                                let _ = surface.update(cx, |_, cx| cx.notify());
                            }
                        },
                    )
                    .size_full(),
                ),
            );
        }
        let incoming_opacity = self
            .displayed
            .as_ref()
            .map_or(0.0, |current| current.preferences.opacity.clamp(0.0, 1.0))
            * progress;
        if let Some((old, _)) = &self.outgoing {
            // Fade both layers: transparent pixels and letterbox gaps must also release the old image smoothly.
            let opacity = old.preferences.opacity.clamp(0.0, 1.0) * (1.0 - progress);
            layer = layer.child(old.element(opacity, false));
        }
        if let Some(current) = &self.displayed {
            layer = layer.child(current.element(incoming_opacity, current.key == key));
        }
        layer
    }
}

impl Presented {
    fn element(&self, opacity: f32, advance: bool) -> AnyElement {
        let mut preferences = self.preferences.clone();
        preferences.opacity = opacity;
        match &self.picture {
            Picture::Image(image) => background_image_layer(preferences, Some(image.clone())),
            Picture::Stream(player) => {
                let player = player.clone();
                div()
                    .absolute()
                    .inset_0()
                    .opacity(opacity)
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                if !bounds.intersects(&window.content_mask().bounds) {
                                    return;
                                }
                                player.update(cx, |player, cx| {
                                    if advance {
                                        player.paint(
                                            bounds,
                                            preferences.alignment,
                                            preferences.fit,
                                            preferences.blur,
                                            window,
                                            cx,
                                        );
                                    } else {
                                        player.paint_current(
                                            bounds,
                                            preferences.alignment,
                                            preferences.fit,
                                            preferences.blur,
                                            window,
                                        );
                                    }
                                });
                            },
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
        }
    }
}
