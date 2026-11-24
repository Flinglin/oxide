pub(super) fn terminal_cwd_looks_remote(cwd: &str) -> bool {
    cwd.starts_with("/home/")
        || cwd.starts_with("/root/")
        || cwd.starts_with("/srv/")
        || cwd.starts_with("/var/www/")
}

pub(super) fn infer_terminal_ssh_identity_from_buffer(buffer: &str) -> Option<String> {
    let tail_start = buffer
        .char_indices()
        .rev()
        .nth(8000)
        .map(|(index, _)| index)
        .unwrap_or(0);
    buffer[tail_start..]
        .split_whitespace()
        .filter_map(terminal_ssh_identity_candidate)
        .next_back()
}

fn terminal_ssh_identity_candidate(token: &str) -> Option<String> {
    if token.contains('=') {
        return None;
    }
    let end = token
        .char_indices()
        .find_map(|(index, ch)| {
            (matches!(ch, ':' | '~' | '#' | '$' | '>') && token[..index].contains('@'))
                .then_some(index)
        })
        .unwrap_or(token.len());
    let candidate = token[..end].trim_matches(|ch: char| {
        !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@'))
    });
    let (user, host) = candidate.split_once('@')?;
    if !(1..=64).contains(&user.len()) || !(1..=128).contains(&host.len()) {
        return None;
    }
    if !user
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return None;
    }
    let mut host_chars = host.chars();
    if !host_chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
    {
        return None;
    }
    if !host_chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
        return None;
    }
    Some(format!("{user}@{host}"))
}
