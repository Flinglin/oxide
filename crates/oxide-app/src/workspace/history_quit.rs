use gpui::App;

/// All explicit exit paths leave the UI running while the database acknowledges its final changes.
pub(crate) fn request_app_quit(cx: &mut App) {
    // Quit can originate inside a workspace update; release its lease before reading it.
    cx.defer(start_app_quit);
}
fn start_app_quit(cx: &mut App) {
    oxide_desktop_presence::request_quit();
    cx.quit();
}
