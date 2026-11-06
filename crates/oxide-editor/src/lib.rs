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

#[cfg(test)]
mod grammar_fixture {
    use oxide_editor_syntax::{
        LanguageId, PluginGrammar, PluginGrammarSource, SyntaxError, SyntaxSession, SyntaxWork,
    };

    // Included only by test modules. Keep actual plugin packages alive for every
    // parser using them, without adding fixture loading to the production API.
    pub(crate) fn grammar(language: LanguageId) -> &'static PluginGrammar {
        use std::sync::OnceLock;
        type Fixture = (tempfile::TempDir, PluginGrammar);
        static C: OnceLock<Fixture> = OnceLock::new();
        static RUST: OnceLock<Fixture> = OnceLock::new();
        static ELIXIR: OnceLock<Fixture> = OnceLock::new();
        let (slot, bytes): (&OnceLock<Fixture>, &[u8]) = match language {
            LanguageId::C => (
                &C,
                include_bytes!("../../oxide-editor-syntax/tests/fixtures/c.zip"),
            ),
            LanguageId::Rust => (
                &RUST,
                include_bytes!("../../oxide-editor-syntax/tests/fixtures/rust.zip"),
            ),
            LanguageId::Elixir => (
                &ELIXIR,
                include_bytes!("../../oxide-editor-syntax/tests/fixtures/elixir.zip"),
            ),
            _ => panic!("No plugin fixture for {language:?}"),
        };
        &slot
            .get_or_init(|| {
                let directory = tempfile::tempdir().unwrap();
                zip::ZipArchive::new(std::io::Cursor::new(bytes))
                    .unwrap()
                    .extract(directory.path())
                    .unwrap();
                let manifest: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(directory.path().join("plugin.json")).unwrap(),
                )
                .unwrap();
                let declared = &manifest["contributes"]["language"];
                let grammar = PluginGrammar::new(PluginGrammarSource {
                    grammar_name: declared["id"].as_str().unwrap().into(),
                    language,
                    parser: directory.path().join("parser.wasm"),
                    highlights: directory.path().join("highlights.scm"),
                    parser_sha256: declared["parserSha256"].as_str().unwrap().into(),
                    highlights_sha256: declared["highlightsSha256"].as_str().unwrap().into(),
                    injections: Vec::new(),
                });
                (directory, grammar)
            })
            .1
    }

    pub(crate) fn parse(language: LanguageId, source: &str) -> Result<SyntaxSession, SyntaxError> {
        parse_controlled(language, source, None)
    }

    pub(crate) fn parse_controlled(
        language: LanguageId,
        source: &str,
        work: Option<&SyntaxWork>,
    ) -> Result<SyntaxSession, SyntaxError> {
        if language.plugin_key().is_some() {
            SyntaxSession::parse_plugin(grammar(language), source, work)
        } else {
            SyntaxSession::parse_controlled(language, source, work)
        }
    }

    pub(crate) fn install_rust(cx: &mut gpui::App) {
        crate::EditorLanguagePlugins::update(vec![grammar(LanguageId::Rust).source().clone()], cx);
    }
}

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
