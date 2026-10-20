use gpui::App;

/// All explicit exit paths leave the UI running while the database acknowledges its final changes.
pub(crate) fn request_app_quit(cx: &mut App) {
    oxide_desktop_presence::request_quit();
    cx.quit();
}
