use std::borrow::Cow;

/// Only embedded program sources pass this boundary, before user paths or
/// configuration are interpolated. Existing checkouts may still contain CRLF
/// even after the repository starts enforcing LF for fresh checkouts.
pub(crate) fn normalize_integration_script(source: &'static str) -> Cow<'static, str> {
    if source.contains("\r\n") {
        Cow::Owned(source.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(source)
    }
}
