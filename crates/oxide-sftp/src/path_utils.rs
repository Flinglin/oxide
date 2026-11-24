use std::{borrow::Cow, collections::HashSet, path::PathBuf};

use crate::SftpError;

pub fn is_absolute_remote_path(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    if path.len() >= 3 {
        let bytes = path.as_bytes();
        return bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\');
    }
    false
}

pub fn join_local_path(base: &str, component: &str) -> String {
    let mut path = PathBuf::from(base);
    path.push(component);
    path.to_string_lossy().to_string()
}

pub(crate) fn validate_remote_entry_name(name: &str) -> Result<(), SftpError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\n', '\r', '\0'])
    {
        return Err(SftpError::ProtocolError(
            "SFTP directory entry is not a safe path component".to_string(),
        ));
    }
    Ok(())
}

pub fn join_remote_path(base: &str, component: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{component}")
    } else {
        format!("{base}/{component}")
    }
}

/// Normalizes a remote path to the absolute forward-slash form used by SFTP.
pub fn normalize_remote_path(path: &str) -> Cow<'_, str> {
    // trim 产生的是原串的子切片，生命周期仍是 'a，可安全借用
    let trimmed = path.trim();

    if trimmed.is_empty() {
        return Cow::Borrowed("/"); // 'static，零分配
    }

    // 快路径：已规范化则直接借用
    if is_normalized_remote(trimmed) {
        return Cow::Borrowed(trimmed);
    }

    // 慢路径：一次遍历完成 反斜杠→正斜杠、折叠重复分隔符、补前导 /
    let needs_prefix = !trimmed.starts_with('/') && !trimmed.starts_with('\\');
    let mut out = String::with_capacity(trimmed.len() + needs_prefix as usize);
    let mut prev_slash = false;

    if needs_prefix {
        out.push('/');
        prev_slash = true;
    }

    for ch in trimmed.chars() {
        let c = if ch == '\\' { '/' } else { ch };
        if c == '/' {
            if !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(c);
            prev_slash = false;
        }
    }

    Cow::Owned(out)
}

#[inline]
fn is_normalized_remote(s: &str) -> bool {
    if !s.starts_with('/') {
        return false;
    }
    let bytes = s.as_bytes();
    let mut prev = 0u8;
    for &b in bytes {
        if b == b'\\' {
            return false;
        }
        if b == b'/' && prev == b'/' {
            return false;
        }
        prev = b;
    }
    true
}
/// Returns the parent of an absolute remote path without crossing the root.
pub fn remote_parent_path(path: &str) -> Cow<'_, str> {
    match normalize_remote_path(path) {
        Cow::Borrowed(s) => {
            // s: &'a str，结果可以借它，生命周期仍是 'a
            let trimmed = s.trim_end_matches('/');
            if trimmed.is_empty() {
                return Cow::Borrowed("/");
            }
            match trimmed.rfind('/') {
                Some(0) | None => Cow::Borrowed("/"),
                Some(idx) => Cow::Borrowed(&trimmed[..idx]),
            }
        }
        Cow::Owned(mut s) => {
            // 已分配 String，就地截断，零额外分配
            let new_len = s.trim_end_matches('/').len();
            s.truncate(new_len);
            if s.is_empty() {
                s.push('/');
            } else if let Some(idx) = s.rfind('/') {
                s.truncate(if idx == 0 { 1 } else { idx });
            } else {
                // 防御：理论上 normalize 后必以 '/' 开头，rfind 不会 None
                s.clear();
                s.push('/');
            }
            Cow::Owned(s)
        }
    }
}

/// Builds each directory prefix needed before creating a nested remote path.
pub fn remote_directory_prefixes(path: &str) -> Vec<String> {
    let absolute = path.starts_with('/');
    let components = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (0..components.len())
        .map(|index| {
            let joined = components[..=index].join("/");
            if absolute {
                format!("/{joined}")
            } else {
                joined
            }
        })
        .collect()
}

/// Chooses the first desktop-style numbered name that does not already exist.
pub fn unique_conflict_name<'a>(
    name: &str,
    existing_names: impl IntoIterator<Item = &'a str>,
) -> String {
    let existing_names = existing_names.into_iter().collect::<HashSet<_>>();
    let (base_name, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], &name[index..]),
        _ => (name, ""),
    };

    let mut counter = 1usize;
    loop {
        let candidate = format!("{base_name} ({counter}){extension}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        counter += 1;
    }
}
