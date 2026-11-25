use super::super::*;

use oxide_terminal::background_display_target;

struct BundledWorkspaceBackground {
    file_name: &'static str,
    bytes: &'static [u8],
}

// Bundled gallery assets are installed on startup and protected from user deletion.
const BUNDLED_WORKSPACE_BACKGROUNDS: &[BundledWorkspaceBackground] = &[
    BundledWorkspaceBackground {
        file_name: "oxide-ambient-v1.png",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/backgrounds/oxide-ambient-v1.png"
        )),
    },
    BundledWorkspaceBackground {
        file_name: "oxide-nocturne-v1.webp",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/backgrounds/oxide-nocturne-v1.webp"
        )),
    },
    BundledWorkspaceBackground {
        file_name: "oxide-verdant-v1.webp",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/backgrounds/oxide-verdant-v1.webp"
        )),
    },
];

pub(in crate::workspace) fn ensure_bundled_workspace_backgrounds(
    settings_path: &Path,
) -> Result<()> {
    for background in BUNDLED_WORKSPACE_BACKGROUNDS {
        ensure_bundled_background_image(settings_path, background.file_name, background.bytes)?;
    }
    Ok(())
}

pub(in crate::workspace) fn is_bundled_workspace_background(
    settings_path: &Path,
    image_path: &Path,
) -> bool {
    let background_directory = background_images_directory(settings_path);
    BUNDLED_WORKSPACE_BACKGROUNDS
        .iter()
        .any(|background| image_path == background_directory.join(background.file_name))
}

impl WorkspaceApp {
    pub(in crate::workspace) fn toggle_titlebar_theme(&mut self, cx: &mut Context<Self>) {
        if self.theme_transition_task.is_some() {
            return;
        }
        match oxide_settings_model::next_appearance_scheme(
            self.settings_store.settings(),
            self.system_dark,
        ) {
            Ok(scheme) => self.change_appearance_mode(Some(scheme), cx),
            Err(error) => self.show_appearance_scheme_error(error, cx),
        }
    }

    pub(in crate::workspace) fn show_appearance_scheme_error(
        &mut self,
        error: oxide_settings_model::AppearanceSchemeError,
        cx: &mut Context<Self>,
    ) {
        use oxide_settings_model::AppearanceSchemeError;
        let (message, scheme) = match error {
            AppearanceSchemeError::Unassigned => (
                self.i18n.t("settings_view.appearance.legacy_scheme_hint"),
                None,
            ),
            AppearanceSchemeError::Identical => (
                self.i18n.t("settings_view.appearance.schemes_identical"),
                self.appearance_edit_dark,
            ),
            AppearanceSchemeError::Unavailable(scheme) => (
                self.i18n
                    .t("settings_view.appearance.scheme_unavailable")
                    .replace(
                        "{{scheme}}",
                        &self.i18n.t(if scheme.is_dark() {
                            "settings_view.appearance.system_dark"
                        } else {
                            "settings_view.appearance.system_light"
                        }),
                    ),
                Some(scheme.is_dark()),
            ),
        };
        self.apply_workspace_overlay_intent(
            overlay::WorkspaceOverlayIntent::AppearanceSetup {
                notice: TerminalNotice {
                    title: message,
                    description: None,
                    status_text: None,
                    progress: None,
                    variant: TerminalNoticeVariant::Warning,
                },
                scheme,
                label: self.i18n.t("settings_view.appearance.configure_schemes"),
                workspace: cx.entity().downgrade(),
            },
            cx,
        );
    }

    fn validate_appearance_mode(
        &self,
        scheme: Option<oxide_settings::AppearanceScheme>,
    ) -> Result<(), oxide_settings_model::AppearanceSchemeError> {
        use oxide_settings::AppearanceScheme;
        for candidate in [AppearanceScheme::Light, AppearanceScheme::Dark] {
            if scheme.is_none_or(|scheme| scheme == candidate) {
                oxide_settings_model::validate_appearance_scheme(
                    self.settings_store.settings(),
                    candidate,
                )?;
            }
        }
        Ok(())
    }

    pub(in crate::workspace) fn change_appearance_mode(
        &mut self,
        scheme: Option<oxide_settings::AppearanceScheme>,
        cx: &mut Context<Self>,
    ) {
        if self.theme_transition_task.is_some() {
            return;
        }
        if let Err(error) = self.validate_appearance_mode(scheme) {
            self.show_appearance_scheme_error(error, cx);
            return;
        }
        let appearance = &self.settings_store.settings().appearance;
        if match scheme {
            None => appearance.follow_system_appearance,
            Some(scheme) => {
                !appearance.follow_system_appearance && appearance.manual_scheme == Some(scheme)
            }
        } {
            return;
        }
        let previous = self.theme_tokens;
        self.apply_appearance_mode(scheme, cx);
        if !self.theme_tokens.motion.enabled
            || cx.reduce_motion()
            || previous.ui == self.theme_tokens.ui
        {
            return;
        }

        let duration = oxide_ui::motion::duration(
            &self.theme_tokens,
            oxide_ui::motion::MotionDuration::Overlay,
        );
        let target = self.theme_tokens;
        self.interpolate_theme_colors(previous, target, 0.0);
        let started = cx.background_executor().now();
        // Only repaint UI colors. Persistence, terminal palettes and plugin events
        // are applied once above; background media retains its existing crossfade.
        self.theme_transition_task = Some(cx.spawn(async move |workspace, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(started);
                let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                // Match the background crossfade's smooth start and finish.
                let eased = progress * progress * (3.0 - 2.0 * progress);
                if workspace
                    .update(cx, |this, cx| {
                        this.interpolate_theme_colors(previous, target, eased);
                        if progress >= 1.0 {
                            this.theme_transition_task = None;
                        }
                        cx.notify();
                    })
                    .is_err()
                    || progress >= 1.0
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn interpolate_theme_colors(&mut self, from: ThemeTokens, to: ThemeTokens, progress: f32) {
        use oxide_ui::motion::lerp;
        self.theme_tokens.ui = from.ui.interpolate(to.ui, progress);
        self.theme_tokens.metrics.window_vibrancy_tint_alpha = lerp(
            from.metrics.window_vibrancy_tint_alpha,
            to.metrics.window_vibrancy_tint_alpha,
            progress,
        );
        self.theme_tokens.metrics.panel_vibrancy_alpha = lerp(
            from.metrics.panel_vibrancy_alpha,
            to.metrics.panel_vibrancy_alpha,
            progress,
        );
        self.theme_tokens.metrics.sidebar_vibrancy_alpha = lerp(
            from.metrics.sidebar_vibrancy_alpha,
            to.metrics.sidebar_vibrancy_alpha,
            progress,
        );
    }

    fn apply_appearance_mode(
        &mut self,
        scheme: Option<oxide_settings::AppearanceScheme>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.validate_appearance_mode(scheme) {
            self.show_appearance_scheme_error(error, cx);
            return;
        }
        self.edit_settings(
            |settings| oxide_settings_model::apply_appearance_mode(settings, scheme),
            cx,
        );
    }
    pub(in crate::workspace) fn sync_system_appearance(
        &mut self,
        appearance: gpui::WindowAppearance,
        cx: &mut Context<Self>,
    ) {
        let dark = matches!(
            appearance,
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        if self.system_dark == dark {
            return;
        }
        let previous_theme = oxide_settings_model::ThemeTarget::Application
            .resolved_id(self.settings_store.settings(), self.system_dark)
            .to_string();
        self.system_dark = dark;
        self.theme_transition_task = None;
        let settings = self.settings_store.settings();
        self.theme_tokens = ui_tokens_from_settings(settings, dark);
        self.active_background = settings.resolved_background(dark);
        // Appearance events update visual state without saving settings or resetting PTYs.
        let panes: Vec<_> = self
            .tab_host
            .read(cx)
            .panes()
            .iter()
            .map(|(id, pane)| (*id, pane.clone()))
            .collect();
        for (id, pane) in panes {
            let preferences = self.terminal_preferences_for_pane(id, cx);
            pane.update(cx, |pane, cx| {
                pane.set_appearance(
                    preferences.theme,
                    preferences.background,
                    preferences.transparent_background,
                    cx,
                )
            });
        }
        self.sync_terminal_command_sender_appearance(cx);
        self.sync_search_editor_appearance(cx);
        self.apply_ide_runtime_settings_to_surfaces(cx);
        let theme = oxide_settings_model::ThemeTarget::Application
            .resolved_id(self.settings_store.settings(), dark)
            .to_string();
        if theme != previous_theme {
            self.emit_native_plugin_event_to_subscribers(plugin_host::NATIVE_PLUGIN_APP_THEME_CHANGED_EVENT,
                serde_json::json!({ "theme": crate::workspace::plugin_lifecycle::native_plugin_theme_snapshot(&theme) }), cx);
        }
        cx.notify();
    }

    pub(in crate::workspace) fn render_workspace_window_background(
        &mut self,
        window_background: &Entity<window_shell::WorkspaceWindowBackgroundEntity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let background = self.window_background_preferences()?;
        Some(self.render_workspace_background_layer(window_background, background, window, cx))
    }

    pub(in crate::workspace) fn wrap_content_background(
        &mut self,
        window_background: &Entity<window_shell::WorkspaceWindowBackgroundEntity>,
        content: AnyElement,
        background_key: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(background_key) = background_key else {
            return if self.window_background_preferences().is_some() {
                oxide_ui::surface::tab_surface(&self.theme_tokens, content)
            } else {
                content
            };
        };
        if matches!(
            background_key,
            "terminal" | "local_terminal" | "remote_desktop"
        ) {
            return content;
        }
        let content = if self.background_surface_active(background_key) {
            oxide_ui::surface::tab_surface(&self.theme_tokens, content)
        } else {
            content
        };
        let Some(background) = self.terminal_background_preferences(background_key) else {
            return content;
        };
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .child(self.render_workspace_background_layer(
                window_background,
                background,
                window,
                cx,
            ))
            .child(div().relative().size_full().child(content))
            .into_any_element()
    }

    fn render_workspace_background_layer(
        &mut self,
        window_background: &Entity<window_shell::WorkspaceWindowBackgroundEntity>,
        mut background: TerminalBackgroundPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let byte_limit = self.render_policy.image_cache_bytes;
        background.on_failure = Some(self.background_failure_handler(cx));
        window_background.update(cx, |window_background, cx| {
            window_background.cache.set_byte_limit(byte_limit);
            window_background.render_layer(background, window, cx)
        })
    }
}

pub(in crate::workspace) fn background_preferences_for_style(
    style: &oxide_settings::BackgroundStyle,
    tokens: &ThemeTokens,
) -> Option<TerminalBackgroundPreferences> {
    if !style.enabled {
        return None;
    }
    let effect = style
        .effect
        .as_ref()
        .map(|effect| oxide_background::GeneratedEffectPreferences {
            kind: match effect.kind {
                oxide_settings::GeneratedBackgroundKind::Mineral => {
                    oxide_background::GeneratedEffectKind::Mineral
                }
                oxide_settings::GeneratedBackgroundKind::Fog => {
                    oxide_background::GeneratedEffectKind::Fog
                }
                oxide_settings::GeneratedBackgroundKind::Tide => {
                    oxide_background::GeneratedEffectKind::Tide
                }
                oxide_settings::GeneratedBackgroundKind::TideParticles => {
                    oxide_background::GeneratedEffectKind::TideParticles
                }
                oxide_settings::GeneratedBackgroundKind::Meteor => {
                    oxide_background::GeneratedEffectKind::Meteor
                }
                oxide_settings::GeneratedBackgroundKind::Particles => {
                    oxide_background::GeneratedEffectKind::Particles
                }
                oxide_settings::GeneratedBackgroundKind::Caustics => {
                    oxide_background::GeneratedEffectKind::Caustics
                }
            },
            strength: effect.strength,
            sheen: effect.sheen,
            max_fps: effect.max_fps,
            colors: effect
                .colors
                .unwrap_or([tokens.ui.accent, tokens.terminal.cyan]),
            speed: if tokens.motion.enabled && tokens.motion.spatial_enabled {
                effect.speed
            } else {
                0.0
            },
            size: effect.size,
            brightness: effect.brightness,
            roughness: effect.roughness,
            direction: effect.direction,
            particle_count: effect.particle_count,
            preview_viewport: None,
        });
    let path = match (&style.image, effect.is_some()) {
        (Some(path), _) => PathBuf::from(path),
        (None, true) => PathBuf::new(),
        (None, false) => return None,
    };
    Some(TerminalBackgroundPreferences {
        scene: oxide_background::ScenePreferences {
            pause_on_input: style.pause_on_input,
            parallax: style.parallax && tokens.motion.enabled && tokens.motion.spatial_enabled,
            camera: style
                .camera
                .filter(|_| tokens.motion.enabled && tokens.motion.spatial_enabled)
                .map(|camera| oxide_background::CameraPreferences {
                    motion: match camera.motion {
                        oxide_settings::BackgroundCameraMotion::Pan => {
                            oxide_background::CameraMotion::Pan
                        }
                        oxide_settings::BackgroundCameraMotion::ZoomIn => {
                            oxide_background::CameraMotion::ZoomIn
                        }
                        oxide_settings::BackgroundCameraMotion::ZoomOut => {
                            oxide_background::CameraMotion::ZoomOut
                        }
                    },
                    amount: camera.amount,
                    speed: camera.speed,
                }),
            day_cycle: style.day_cycle,
            dark: {
                let color = tokens.terminal.background;
                ((color >> 16 & 255) * 299 + (color >> 8 & 255) * 587 + (color & 255) * 114)
                    < 128000
            },
            ..Default::default()
        },
        path,
        effect,
        readability: (style.readability > 0.0).then_some(oxide_background::ReadingOverlay {
            color: tokens.terminal.background,
            opacity: style.readability.clamp(0.0, 1.0),
        }),
        alignment: style.alignment.position(),
        opacity: style.opacity.clamp(0.0, 1.0) as f32,
        blur: style.blur.clamp(0, 20) as f32,
        fit: terminal_background_fit(style.fit),
        limits: oxide_background::PlaybackLimits {
            max_width: style.max_width,
            max_height: style.max_height,
            max_fps: style.max_fps,
        },
        on_failure: None,
    })
}

impl window_shell::WorkspaceWindowBackgroundEntity {
    fn render_layer(
        &mut self,
        background: TerminalBackgroundPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display = background_display_target(window.bounds().size, window.scale_factor());
        let image = if oxide_background::is_streaming_source(&background.path) {
            None
        } else {
            self.cache.render_background_image(&background, display)
        };
        self.drop_retired_images(Some(window), cx);
        if self.cache.has_pending() {
            self.schedule_decode_completion(cx);
        }
        oxide_background::background_layer(background, image, window, cx)
    }

    fn schedule_decode_completion(&mut self, cx: &mut Context<Self>) {
        if self.decode_completion_task.is_some() {
            return;
        }
        // Each shell owns its cache completion task, so releasing one native
        // window cannot keep repaint work alive through the shared session.
        self.decode_completion_task = Some(cx.spawn(async move |window_background, cx| {
            Timer::after(Duration::from_millis(16)).await;
            let _ = window_background.update(cx, |window_background, cx| {
                window_background.decode_completion_task = None;
                if window_background.cache.drain_completed() {
                    window_background.drop_retired_images(None, cx);
                    cx.notify();
                }
                if window_background.cache.has_pending() {
                    window_background.schedule_decode_completion(cx);
                }
            });
        }));
    }

    fn drop_retired_images(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        for image in self.cache.take_retired_images() {
            // RenderImage entries painted by gpui::img also stay in the atlas
            // until the app explicitly drops their image id.
            if let Some(window) = window.as_mut() {
                cx.drop_image(image, Some(*window));
            } else {
                cx.drop_image(image, None);
            }
        }
    }
}

pub(in crate::workspace) fn default_connections_path() -> PathBuf {
    default_settings_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("connections.json")
}

pub(in crate::workspace) fn default_saved_forwards_path() -> PathBuf {
    default_settings_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("forwards.json")
}

pub(in crate::workspace) fn default_session_tree_path() -> PathBuf {
    default_settings_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("session_tree.json")
}
