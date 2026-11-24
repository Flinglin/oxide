//! GPUI text editor surface for oxide's native IDE path.
//!
//! This crate deliberately starts with virtualized plain-text rendering only.
//! Syntax, project ownership, and remote save semantics belong to later crates
//! in `docs/native-editor-ide-plan.md`.

mod languages;
mod metrics;
mod settings;
mod surface;
mod viewport;

pub use languages::{
    EditorLanguageDefinition, EditorLanguagePlugins, ManageLanguagePlugin, detect_language,
    render_language_plugin_notice,
};
pub use metrics::{EditorAppearance, EditorMetrics};
pub use settings::EditorSettings;
pub use surface::{
    EditorCommand, EditorContextMenuLabels, EditorKeybindings, EditorPresentation,
    EditorSaveStatus, EditorScrollAnchor, EditorScrollOrigin, EditorSearchLabels, EditorShortcut,
    EditorViewportChanged, SaveCallback, TextEditorView,
};
pub use viewport::{EditorViewport, VisibleRows};
