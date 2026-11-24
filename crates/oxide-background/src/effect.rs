use crate::clock::PlaybackClock;
use gpui::{
    AnyElement, App, Bounds, BoxShadow, Context, Corners, Hsla, PathBuilder, Pixels, Render,
    Subscription, Task, Window, canvas, div, linear_color_stop, linear_gradient, point, prelude::*,
    px, rgba, size,
};
use std::time::{Duration, Instant};

pub const MIN_EFFECT_DENSITY: u32 = 1;
pub const MAX_EFFECT_DENSITY: u32 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedEffectKind {
    Mineral,
    Fog,
    Tide,
    TideParticles,
    Meteor,
    Particles,
    Caustics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratedEffectPreferences {
    pub kind: GeneratedEffectKind,
    pub strength: f32,
    pub sheen: f32,
    pub max_fps: Option<u32>,
    pub colors: [u32; 2],
    pub speed: f32,
    pub size: f32,
    pub brightness: f32,
    pub roughness: f32,
    pub direction: f32,
    pub particle_count: u32,
    /// Preview density uses the workspace viewport, rather than the small preview card.
    pub preview_viewport: Option<(f32, f32)>,
}

struct EffectSurface {
    preferences: GeneratedEffectPreferences,
    opacity: f32,
    max_fps: u32,
    clock: PlaybackClock,
    visible: bool,
    paused: bool,
    parallax_offset: (f32, f32),
    timer: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn effect_layer(
    preferences: GeneratedEffectPreferences,
    paused: bool,
    parallax_offset: Option<(f32, f32)>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let surface = window
        .use_keyed_state("generated-background", cx, |window, cx| {
            cx.new(|cx| EffectSurface::new(preferences, window, cx))
        })
        .read(cx)
        .clone();
    surface.update(cx, |surface, _| {
        surface.paused = paused;
        surface.parallax_offset = parallax_offset.unwrap_or_default();
        surface.preferences = preferences;
        let strength = preferences.strength.clamp(0.0, 1.0);
        // Use the same response with and without media so previewing cannot change intensity.
        surface.opacity = strength * (2.0 - strength);
        surface.max_fps = preferences.max_fps.unwrap_or(30).clamp(1, 30);
    });
    surface.into_any_element()
}

impl EffectSurface {
    fn new(
        preferences: GeneratedEffectPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe_window_activation(window, |this: &mut EffectSurface, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
            cx.observe_window_visibility(window, |this: &mut EffectSurface, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
        ];
        Self {
            preferences,
            opacity: {
                let strength = preferences.strength.clamp(0.0, 1.0);
                strength * (2.0 - strength)
            },
            max_fps: preferences.max_fps.unwrap_or(30).clamp(1, 30),
            clock: PlaybackClock::default(),
            visible: false,
            paused: false,
            parallax_offset: (0.0, 0.0),
            timer: None,
            _subscriptions: subscriptions,
        }
    }
    fn sync(&mut self, window: &Window, cx: &mut Context<Self>) {
        let active = self.visible
            && !self.paused
            && self.opacity > 0.0
            && (self.preferences.kind == GeneratedEffectKind::Mineral
                || self.preferences.brightness > 0.0)
            && self.preferences.speed > 0.0
            && (self.preferences.kind != GeneratedEffectKind::Mineral
                || self.preferences.sheen > 0.0)
            && window.is_window_active()
            && window.is_visible()
            && !window.is_minimized()
            && !cx.reduce_motion();
        let now = cx.background_executor().now();
        if self.paused {
            self.clock.pause_for_input(now);
        } else {
            self.clock.set_running(active, now);
        }
        if !active {
            self.timer = None;
            return;
        }
        if self.timer.is_none() {
            let interval = Duration::from_secs_f64(1.0 / self.max_fps as f64);
            // The mounted surface owns this one-shot repaint; unmounting cancels it.
            self.timer = Some(cx.spawn(async move |surface, cx| {
                cx.background_executor().timer(interval).await;
                let _ = surface.update(cx, |surface, cx| {
                    surface.timer = None;
                    cx.notify();
                });
            }));
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, now: Instant) {
        if bounds.is_empty()
            || self.opacity <= 0.0
            || (self.preferences.kind != GeneratedEffectKind::Mineral
                && self.preferences.brightness <= 0.0)
        {
            return;
        }
        let phase =
            self.clock.position(now).as_secs_f32() * self.preferences.speed.clamp(0.0, 3.0) * 0.07;
        match self.preferences.kind {
            GeneratedEffectKind::Fog => paint_fog(bounds, self.preferences, phase, window),
            GeneratedEffectKind::Tide => paint_tide(bounds, self.preferences, phase, window),
            GeneratedEffectKind::TideParticles => {
                // Leave luminance headroom for the particle response above the light field.
                paint_tide(
                    bounds,
                    GeneratedEffectPreferences {
                        brightness: self.preferences.brightness * 0.5,
                        ..self.preferences
                    },
                    phase,
                    window,
                );
                paint_particles(
                    bounds,
                    self.preferences,
                    phase,
                    self.parallax_offset,
                    window,
                );
            }
            GeneratedEffectKind::Meteor => paint_meteors(bounds, self.preferences, phase, window),
            GeneratedEffectKind::Particles => paint_particles(
                bounds,
                self.preferences,
                phase,
                self.parallax_offset,
                window,
            ),
            GeneratedEffectKind::Caustics => {
                paint_caustics(bounds, self.preferences, phase, window)
            }
            GeneratedEffectKind::Mineral => {
                let colors: [Hsla; 2] = self
                    .preferences
                    .colors
                    .map(|color| rgba(((color & 0xffffff) << 8) | 0xff).into());
                let background = gpui::procedural_noise(
                    colors,
                    self.preferences.size.clamp(0.3, 2.0)
                        * 3.0
                        * window.scale_factor()
                        * detail_scale(bounds, self.preferences),
                    self.preferences.roughness,
                    phase,
                    self.preferences.sheen,
                );
                window.paint_quad(gpui::fill(bounds, background));
            }
        }
    }
}

impl Render for EffectSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = cx.entity();
        let paint_surface = surface.clone();
        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .opacity(self.opacity)
            .child(
                canvas(
                    move |bounds, window, cx| {
                        surface.update(cx, |surface, cx| {
                            surface.visible = bounds.intersects(&window.content_mask().bounds);
                            surface.sync(window, cx);
                        });
                    },
                    move |bounds, _, window, cx| {
                        paint_surface.update(cx, |surface, cx| {
                            if surface.visible {
                                surface.paint(bounds, window, cx.background_executor().now());
                            }
                        });
                    },
                )
                .size_full(),
            )
    }
}

fn paint_glow(
    bounds: Bounds<Pixels>,
    color: u32,
    alpha: f32,
    blur: f32,
    radius: f32,
    window: &mut Window,
) {
    window.paint_drop_shadows(
        bounds,
        Corners::all(px(radius)),
        &[BoxShadow {
            // A light core keeps the theme hue visible against detailed media.
            color: rgba((color & 0xffffff) << 8)
                .blend(rgba(0xffffff40))
                .alpha(alpha)
                .into(),
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(blur),
            spread_radius: px(0.0),
            inset: false,
        }],
    );
}

fn paint_fog(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    phase: f32,
    window: &mut Window,
) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    // Overlapping, independently breathing fields avoid three rigid circles moving in orbit.
    for index in 0..field_count(bounds, preferences, 64) {
        let seed = index as f32;
        let x = (seed * 0.618034 + 0.1).fract() + 0.16 * (phase * 0.8 + seed).sin();
        let y = (seed * 0.381966 + 0.2).fract() + 0.18 * (phase * 0.61 + seed * 2.3).cos();
        let diameter = width.min(height)
            * preferences.size.clamp(0.3, 2.0)
            * (0.42 + 0.14 * (phase * 0.7 + seed * 1.3).sin());
        let glow = Bounds::new(
            bounds.origin
                + point(
                    px(x * width - diameter * 0.5),
                    px(y * height - diameter * 0.5),
                ),
            size(px(diameter), px(diameter)),
        );
        paint_glow(
            glow,
            preferences.colors[index as usize % 2],
            preferences.brightness.clamp(0.0, 1.0) * (0.65 + 0.2 * (phase + seed).cos()),
            diameter * 0.3,
            diameter * 0.5,
            window,
        );
    }
}

struct TideField {
    center: gpui::Point<Pixels>,
    direction: (f32, f32),
    span: f32,
    band: f32,
    bands: usize,
    phase: f32,
}

impl TideField {
    fn new(bounds: Bounds<Pixels>, preferences: GeneratedEffectPreferences, phase: f32) -> Self {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        Self {
            center: bounds.center(),
            direction: preferences
                .direction
                .clamp(0.0, 360.0)
                .to_radians()
                .sin_cos(),
            span: width.hypot(height).max(1.0),
            band: (width.min(height) * 0.18 * preferences.size.clamp(0.3, 2.0)).max(0.1),
            bands: (preferences.particle_count as f32 / 8.0)
                .ceil()
                .clamp(1.0, 8.0) as usize,
            phase,
        }
    }

    fn cross(&self, layer: usize, along: f32) -> f32 {
        let offset = (layer as f32 + 0.5) / self.bands as f32 - 0.5;
        let travel =
            offset * self.span + (self.phase * 0.8 + layer as f32 * 2.4).sin() * self.span * 0.15;
        travel
            + ((along / self.span * 6.0 + self.phase + layer as f32).sin()
                + 0.35 * (along / self.span * 11.0 - self.phase * 1.3).sin())
                * self.band
    }

    fn point(&self, layer: usize, along: f32) -> gpui::Point<Pixels> {
        let (dy, dx) = self.direction;
        let cross = self.cross(layer, along);
        self.center + point(px(dx * cross - dy * along), px(dy * cross + dx * along))
    }

    fn illumination(&self, position: gpui::Point<Pixels>) -> f32 {
        let delta = position - self.center;
        let (dy, dx) = self.direction;
        let along = -dy * delta.x.as_f32() + dx * delta.y.as_f32();
        let cross = dx * delta.x.as_f32() + dy * delta.y.as_f32();
        // Light interaction follows the very same moving curves that paint the ribbons.
        (0..self.bands)
            .map(|layer| {
                let distance = (cross - self.cross(layer, along)).abs() / (self.band * 0.65);
                let t = (1.0 - distance).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            })
            .fold(0.0, f32::max)
    }
}

fn paint_tide(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    phase: f32,
    window: &mut Window,
) {
    let field = TideField::new(bounds, preferences, phase);
    let band = field.band;
    // Closely overlapping fields make continuous ribbons instead of a row of separate disks.
    for layer in 0..field.bands {
        for segment in -8..=8 {
            let center = field.point(layer, segment as f32 * field.span / 16.0);
            let glow = Bounds::new(
                center - point(px(band), px(band)),
                size(px(band * 2.0), px(band * 2.0)),
            );
            paint_glow(
                glow,
                preferences.colors[layer % 2],
                preferences.brightness.clamp(0.0, 1.0) * 0.65,
                band * 0.4,
                band,
                window,
            );
        }
    }
}

fn field_count(bounds: Bounds<Pixels>, preferences: GeneratedEffectPreferences, cap: u32) -> u32 {
    let (width, height) = preferences
        .preview_viewport
        .unwrap_or((bounds.size.width.as_f32(), bounds.size.height.as_f32()));
    let area = (width * height / (1280.0 * 720.0)).clamp(0.5, 4.0);
    (preferences
        .particle_count
        .clamp(MIN_EFFECT_DENSITY, MAX_EFFECT_DENSITY) as f32
        * area)
        .round()
        .clamp(1.0, cap as f32) as u32
}

fn detail_scale(bounds: Bounds<Pixels>, preferences: GeneratedEffectPreferences) -> f32 {
    let short = bounds.size.width.as_f32().min(bounds.size.height.as_f32());
    let reference = preferences
        .preview_viewport
        .map_or(short, |(w, h)| w.min(h));
    // Cap physical detail in a full window, then shrink it proportionally inside a preview.
    (reference / 720.0).clamp(0.5, 1.5) * short / reference.max(1.0)
}

struct Particle {
    center: gpui::Point<Pixels>,
    radius: f32,
    alpha: f32,
    blur: f32,
    core: f32,
}

impl Particle {
    fn light(&mut self, amount: f32) {
        self.alpha *= 0.5 + 0.5 * amount;
        self.radius *= 1.0 + 0.16 * amount;
        self.blur *= 1.0 + 0.25 * amount;
    }
}

fn particle_depth(index: u32) -> usize {
    match index % 6 {
        0..=2 => 0,
        3..=4 => 1,
        _ => 2,
    }
}

fn particle_at(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    index: u32,
    phase: f32,
    offset: (f32, f32),
) -> Particle {
    // More distant points keep the field legible; only a few foreground lights are defocused.
    let (scale, speed, parallax, blur, core) = match particle_depth(index) {
        0 => (0.55, 0.045, 0.35, 0.25, 1.0),
        1 => (1.35, 0.11, 1.2, 0.7, 0.55),
        _ => (3.4, 0.21, 2.8, 1.1, 0.0),
    };
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let seed = index as f32;
    let progress = (field_random(index.wrapping_mul(2)) + phase * speed).fract();
    let drift = (phase * speed * 3.0 + seed * 1.7).sin() * 0.018 * scale;
    let x = field_random(index.wrapping_mul(2).wrapping_add(1)) + drift;
    let radius = 2.8 * detail_scale(bounds, preferences) * preferences.size.clamp(0.3, 2.0) * scale;
    let preview_scale = preferences
        .preview_viewport
        .map_or(1.0, |(w, h)| width.min(height) / w.min(h).max(1.0));
    Particle {
        center: bounds.origin
            + point(
                px(x * width + offset.0 * parallax * preview_scale),
                px((1.05 - progress * 1.1) * height + offset.1 * parallax * preview_scale),
            ),
        radius,
        alpha: preferences.brightness.clamp(0.0, 1.0)
            * (progress.min(1.0 - progress) * 10.0).min(1.0)
            * (0.8 + 0.2 * (phase * 2.0 + seed).sin()),
        blur: radius * blur,
        core,
    }
}

fn paint_particles(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    phase: f32,
    offset: (f32, f32),
    window: &mut Window,
) {
    let count = field_count(bounds, preferences, 512);
    let tide = (preferences.kind == GeneratedEffectKind::TideParticles)
        .then(|| TideField::new(bounds, preferences, phase));
    // Paint back to front so distant cores never sit on top of foreground bokeh.
    for depth in 0..3 {
        for index in (0..count).filter(|index| particle_depth(*index) == depth) {
            let mut particle = particle_at(bounds, preferences, index, phase, offset);
            if let Some(tide) = &tide {
                particle.light(tide.illumination(particle.center));
            }
            let radius = particle.radius;
            let dot = Bounds::new(
                particle.center - point(px(radius), px(radius)),
                size(px(radius * 2.0), px(radius * 2.0)),
            );
            let color = preferences.colors[index as usize % 2];
            paint_glow(dot, color, particle.alpha, particle.blur, radius, window);
            // Distant dust has a defined core; the foreground remains softly out of focus.
            if particle.core > 0.0 {
                window.paint_quad(
                    gpui::fill(
                        dot,
                        rgba((color << 8) | 255)
                            .blend(rgba(0xffffff99))
                            .alpha(particle.alpha * particle.core),
                    )
                    .corner_radii(px(radius)),
                );
            }
        }
    }
}

fn paint_caustics(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    phase: f32,
    window: &mut Window,
) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let scale = preferences.size.clamp(0.3, 2.0);
    let density = (preferences
        .particle_count
        .clamp(MIN_EFFECT_DENSITY, MAX_EFFECT_DENSITY) as f32
        / 12.0)
        .sqrt();
    let lines = (5.0 * density / scale).round().clamp(2.0, 24.0) as usize;
    let stroke = 2.0 * detail_scale(bounds, preferences);
    let span = width.hypot(height) * 1.2;
    // Two gently warped families form an open light mesh; no background readback is needed.
    for family in 0..2 {
        let angle: f32 = if family == 0 { 0.45 } else { 1.85 };
        let (sin, cos) = angle.sin_cos();
        for line in 0..=lines {
            let seed = line as f32 * 1.73 + family as f32 * 2.41;
            let base = line as f32 / lines as f32;
            for (spread, opacity) in [(4.0, 0.22), (1.0, 0.85)] {
                let mut path = PathBuilder::stroke(px(stroke * spread));
                for segment in 0..=32 {
                    let along = segment as f32 / 32.0;
                    let breathing = 0.8 + 0.3 * (phase * 0.9 + seed).sin();
                    let wave = ((along * 9.0 / scale + phase * 0.8 + seed).sin() * 0.075
                        + (along * 17.0 / scale - phase * 0.51 + seed * 2.0).sin() * 0.035)
                        * breathing;
                    let cross = base + wave + (phase * 0.23 + seed).sin() * 0.035;
                    let x = (along - 0.5) * span;
                    let y = (cross - 0.5) * span;
                    let point =
                        bounds.center() + point(px(x * cos - y * sin), px(x * sin + y * cos));
                    if segment == 0 {
                        path.move_to(point);
                    } else {
                        path.line_to(point);
                    }
                }
                let color = rgba((preferences.colors[family] << 8) | 255).blend(rgba(0xffffff88));
                window.paint_path(
                    path.build().expect("caustic curve has finite coordinates"),
                    color.alpha(opacity * preferences.brightness.clamp(0.0, 1.0)),
                );
            }
        }
    }
}

fn field_random(seed: u32) -> f32 {
    // Integer mixing avoids the visible diagonals produced by correlated fractional sequences.
    let mut bits = seed.wrapping_add(0x9e3779b9);
    bits = (bits ^ (bits >> 16)).wrapping_mul(0x21f0aaad);
    bits = (bits ^ (bits >> 15)).wrapping_mul(0x735a2d97);
    ((bits ^ (bits >> 15)) >> 8) as f32 / 16_777_216.0
}

struct Meteor {
    head: gpui::Point<Pixels>,
    tail: gpui::Point<Pixels>,
    alpha: f32,
    velocity: f32,
}

fn meteor_at(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    index: u32,
    phase: f32,
) -> Meteor {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let (dy, dx) = preferences
        .direction
        .clamp(0.0, 360.0)
        .to_radians()
        .sin_cos();
    let seed = index * 4;
    let length =
        width.min(height) * preferences.size.clamp(0.3, 2.0) * (0.16 + 0.16 * field_random(seed));
    let rate = 1.4 + field_random(seed + 1);
    let timeline = phase * rate + field_random(seed + 2);
    let progress = timeline.rem_euclid(1.0);
    let cycle = timeline.floor() as i64 as u32;
    let lane = (field_random((seed + 3).wrapping_add(cycle.wrapping_mul(1024))) - 0.5) * 0.95;
    let across = lane * (dy.abs() * width + dx.abs() * height);
    let span = width.hypot(height) + length * 2.0;
    let travel = (progress - 0.5) * span;
    let head =
        bounds.center() + point(px(dx * travel - dy * across), px(dy * travel + dx * across));
    Meteor {
        head,
        tail: head - point(px(dx * length), px(dy * length)),
        alpha: preferences.brightness.clamp(0.0, 1.0)
            * (progress.min(1.0 - progress) * 6.0).min(1.0),
        velocity: span * rate,
    }
}

const MAX_METEOR_SPARKS: u32 = 256;
const SPARK_LIFETIME: f32 = 0.065;

fn meteor_spark(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    index: u32,
    spark: u32,
    count: u32,
    phase: f32,
) -> Particle {
    let seed = index.wrapping_mul(31).wrapping_add(spark);
    let offset = spark as f32 / count as f32 + field_random(index) * 0.2;
    let birth = ((phase / SPARK_LIFETIME - offset).floor() + offset) * SPARK_LIFETIME;
    let age = (phase - birth).clamp(0.0, SPARK_LIFETIME);
    let t = age / SPARK_LIFETIME;
    let parent = meteor_at(bounds, preferences, index, birth);
    let (dy, dx) = preferences
        .direction
        .clamp(0.0, 360.0)
        .to_radians()
        .sin_cos();
    let radius = detail_scale(bounds, preferences) * (0.65 + field_random(seed) * 0.6);
    // Integrate exponential drag so fragments continue forward while steadily losing speed.
    let travel = parent.velocity * 0.35 * 0.02 * (1.0 - (-age / 0.02).exp());
    let spread = (field_random(seed.wrapping_add(101)) - 0.5) * radius * 18.0 * t;
    let origin = parent.tail + (parent.head - parent.tail) * 0.25;
    Particle {
        center: origin + point(px(dx * travel - dy * spread), px(dy * travel + dx * spread)),
        radius,
        alpha: parent.alpha * (t * 8.0).min(1.0) * (1.0 - t).powi(2) * 0.8,
        blur: radius * 1.2,
        core: 0.65,
    }
}

fn paint_meteors(
    bounds: Bounds<Pixels>,
    preferences: GeneratedEffectPreferences,
    phase: f32,
    window: &mut Window,
) {
    let angle = preferences.direction.clamp(0.0, 360.0);
    let (dy, dx) = angle.to_radians().sin_cos();
    let radius = 2.0 * detail_scale(bounds, preferences);
    let count = field_count(bounds, preferences, 256);
    let sparks_per_meteor = (MAX_METEOR_SPARKS / count).clamp(1, 4);
    for index in 0..count {
        let Meteor {
            head, tail, alpha, ..
        } = meteor_at(bounds, preferences, index, phase);
        let normal = point(px(-dy * radius), px(dx * radius));
        let color = (preferences.colors[index as usize % 2] & 0xffffff) << 8;
        let highlight = rgba(color).blend(rgba(0xffffff66));
        let mut path = PathBuilder::fill();
        path.move_to(tail);
        path.line_to(head + normal);
        path.line_to(head - normal);
        path.close();
        window.paint_path(
            path.build().expect("meteor tail is a finite triangle"),
            linear_gradient(
                (angle + 90.0) % 360.0,
                linear_color_stop(highlight.alpha(0.0), 0.0),
                linear_color_stop(highlight.alpha(alpha), 1.0),
            ),
        );
        let glow = Bounds::new(
            head - point(px(radius), px(radius)),
            size(px(radius * 2.0), px(radius * 2.0)),
        );
        paint_glow(
            glow,
            preferences.colors[index as usize % 2],
            alpha * 0.85,
            radius * 4.0,
            radius,
            window,
        );
        window.paint_quad(gpui::fill(glow, rgba(0xffffffff).alpha(alpha)).corner_radii(px(radius)));
        for spark in 0..sparks_per_meteor {
            let spark = meteor_spark(bounds, preferences, index, spark, sparks_per_meteor, phase);
            let glow = Bounds::new(
                spark.center - point(px(spark.radius), px(spark.radius)),
                size(px(spark.radius * 2.0), px(spark.radius * 2.0)),
            );
            paint_glow(
                glow,
                preferences.colors[index as usize % 2],
                spark.alpha,
                spark.blur,
                spark.radius,
                window,
            );
            window.paint_quad(
                gpui::fill(
                    glow,
                    rgba(color | 255)
                        .blend(rgba(0xffffff66))
                        .alpha(spark.alpha * spark.core),
                )
                .corner_radii(px(spark.radius)),
            );
        }
    }
}
