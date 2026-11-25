#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SystemThemeSettings {
    pub light: String,
    pub dark: String,
}

impl Default for SystemThemeSettings {
    fn default() -> Self {
        Self {
            light: "paper-oxide".into(),
            dark: "magnetite".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundAlignment {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl BackgroundAlignment {
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    pub fn position(self) -> (f32, f32) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::Right => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GeneratedBackgroundSettings {
    pub kind: GeneratedBackgroundKind,
    pub strength: f32,
    pub sheen: f32,
    pub max_fps: Option<u32>,
    pub speed: f32,
    pub size: f32,
    pub brightness: f32,
    pub colors: Option<[u32; 2]>,
    pub roughness: f32,
    pub direction: f32,
    pub particle_count: u32,
}

impl Default for GeneratedBackgroundSettings {
    fn default() -> Self {
        Self {
            kind: GeneratedBackgroundKind::Fog,
            strength: 0.6,
            sheen: 0.0,
            max_fps: None,
            speed: 1.0,
            size: 1.0,
            brightness: 0.75,
            colors: None,
            roughness: 0.6,
            direction: 25.0,
            particle_count: 12,
        }
    }
}

impl GeneratedBackgroundSettings {
    pub fn has_motion(&self) -> bool {
        self.kind != GeneratedBackgroundKind::Mineral || self.sheen > 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GeneratedBackgroundKind {
    Mineral,
    #[default]
    Fog,
    Tide,
    TideParticles,
    Meteor,
    Particles,
    Caustics,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundCameraMotion {
    #[default]
    Pan,
    ZoomIn,
    ZoomOut,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackgroundCameraSettings {
    pub motion: BackgroundCameraMotion,
    pub amount: f32,
    pub speed: f32,
}

impl Default for BackgroundCameraSettings {
    fn default() -> Self {
        Self {
            motion: BackgroundCameraMotion::Pan,
            amount: 0.5,
            speed: 1.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackgroundStyle {
    pub enabled: bool,
    pub image: Option<String>,
    pub opacity: f64,
    pub blur: i64,
    pub fit: BackgroundFit,
    pub alignment: BackgroundAlignment,
    pub effect: Option<GeneratedBackgroundSettings>,
    pub readability: f32,
    pub pause_on_input: bool,
    pub parallax: bool,
    pub camera: Option<BackgroundCameraSettings>,
    pub day_cycle: bool,
    pub scope: BackgroundScope,
    pub enabled_tabs: Vec<String>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_fps: Option<u32>,
}

impl Default for BackgroundStyle {
    fn default() -> Self {
        TerminalSettings::default().background_style()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SystemBackgroundSettings {
    pub light: Option<BackgroundStyle>,
    pub dark: Option<BackgroundStyle>,
}

impl TerminalSettings {
    pub fn background_style(&self) -> BackgroundStyle {
        BackgroundStyle {
            enabled: self.background_enabled,
            image: self.background_image.clone(),
            opacity: self.background_opacity,
            blur: self.background_blur,
            fit: self.background_fit,
            alignment: self.background_alignment,
            effect: self.background_effect.clone(),
            readability: self.background_readability,
            pause_on_input: self.background_pause_on_input,
            parallax: self.background_parallax,
            camera: self.background_camera,
            day_cycle: self.background_day_cycle,
            scope: self.background_scope,
            enabled_tabs: self.background_enabled_tabs.clone(),
            max_width: self.background_max_width,
            max_height: self.background_max_height,
            max_fps: self.background_max_fps,
        }
    }

    pub fn background_for_scheme(&self, dark: Option<bool>) -> BackgroundStyle {
        let selected = match dark {
            Some(true) => &self.system_backgrounds.dark,
            Some(false) => &self.system_backgrounds.light,
            None => return self.background_style(),
        };
        selected.clone().unwrap_or_else(|| self.background_style())
    }

    pub fn apply_background_style(&mut self, style: BackgroundStyle) {
        self.background_enabled = style.enabled;
        self.background_image = style.image;
        self.background_opacity = style.opacity;
        self.background_blur = style.blur;
        self.background_fit = style.fit;
        self.background_alignment = style.alignment;
        self.background_effect = style.effect;
        self.background_readability = style.readability;
        self.background_pause_on_input = style.pause_on_input;
        self.background_parallax = style.parallax;
        self.background_camera = style.camera;
        self.background_day_cycle = style.day_cycle;
        self.background_scope = style.scope;
        self.background_enabled_tabs = style.enabled_tabs;
        self.background_max_width = style.max_width;
        self.background_max_height = style.max_height;
        self.background_max_fps = style.max_fps;
    }

    pub fn set_background_for_scheme(&mut self, dark: Option<bool>, style: BackgroundStyle) {
        match dark {
            Some(true) => self.system_backgrounds.dark = Some(style),
            Some(false) => self.system_backgrounds.light = Some(style),
            None => self.apply_background_style(style),
        }
    }
}

impl PersistedSettings {
    pub fn resolved_background(&self, dark: bool) -> BackgroundStyle {
         self.terminal.background_for_scheme(
            self.appearance
                .active_scheme(dark)
                .map(AppearanceScheme::is_dark),
        )
    }
}
