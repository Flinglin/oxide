use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf, Prefix},
};

use crate::{LocalSidebarLocation, LocalSidebarLocationKind};

#[cfg(target_os = "macos")]
const MACOS_APPLICATIONS_PATH: &str = "/Applications";

pub fn home_path() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| {
            #[cfg(windows)]
            {
                "C:\\".to_string()
            }
            #[cfg(not(windows))]
            {
                "/".to_string()
            }
        })
}

pub fn default_download_path() -> String {
    select_default_download_path(dirs::download_dir(), std::env::temp_dir(), Path::is_dir)
        .to_string_lossy()
        .to_string()
}

fn select_default_download_path(
    download_directory: Option<PathBuf>,
    fallback_directory: PathBuf,
    is_directory: impl Fn(&Path) -> bool,
) -> PathBuf {
    // Prefer the operating system's discoverable Downloads folder, but never
    // fall back to the user's home directory where configuration files live.
    download_directory
        .filter(|path| is_directory(path))
        .unwrap_or(fallback_directory)
}

pub fn local_sidebar_locations() -> Vec<LocalSidebarLocation> {
    let candidates = [
        (
            LocalSidebarLocationKind::Home,
            dirs::home_dir().or_else(|| Some(PathBuf::from(home_path()))),
        ),
        (
            LocalSidebarLocationKind::Applications,
            platform_applications_path(),
        ),
        (LocalSidebarLocationKind::Desktop, dirs::desktop_dir()),
        (LocalSidebarLocationKind::Documents, dirs::document_dir()),
        (LocalSidebarLocationKind::Downloads, dirs::download_dir()),
    ];
    sidebar_locations_from_candidates(candidates, Path::is_dir)
}

fn sidebar_locations_from_candidates<const N: usize>(
    candidates: [(LocalSidebarLocationKind, Option<PathBuf>); N],
    is_directory: impl Fn(&Path) -> bool,
) -> Vec<LocalSidebarLocation> {
    // Only expose real directories so every sidebar row is immediately navigable.
    candidates
        .into_iter()
        .filter_map(|(kind, path)| path.map(|path| (kind, path)))
        .filter(|(_, path)| is_directory(path))
        .map(|(kind, path)| LocalSidebarLocation {
            kind,
            path: path.to_string_lossy().to_string(),
        })
        .collect()
}

fn platform_applications_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from(MACOS_APPLICATIONS_PATH))
    }
    #[cfg(windows)]
    {
        std::env::var_os("ProgramFiles").map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

pub fn resolve_home_path<'a>(path: &'a str) -> Cow<'a, str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "~" || trimmed == "$HOME" {
        return Cow::Owned(home_path());
    }
    let rest = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
        .or_else(|| trimmed.strip_prefix("$HOME/"))
        .or_else(|| trimmed.strip_prefix("$HOME\\"));

    match rest {
        Some(rest) => {
            let home = home_path();
            let mut out = String::with_capacity(home.len() + 1 + rest.len());
            out.push_str(&home);
            if !home.ends_with(['/', '\\']) {
                out.push(std::path::MAIN_SEPARATOR);
            }
            out.push_str(rest);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(trimmed), // 零分配
    }
}

pub fn local_parent_path<'a>(path: &'a str) -> Option<Cow<'a, str>> {
    let normalized = resolve_home_path(path);
    match normalized {
        // 路径不需要 home 展开：s: &'a str
        Cow::Borrowed(s) => {
            let segments = split_path_into_segments(s); // SegmentedPath<'a>
            let idx = segments.len().saturating_sub(2);
            if let Some(seg) = segments.get_borrowed(idx) {
                Some(Cow::Borrowed(seg.full_path)) // 零分配
            } else {
                // 需要规范化（c:/x、/a/../b 等），转 Owned
                segments
                    .get(idx)
                    .map(|seg| Cow::Owned(seg.full_path.to_owned()))
            }
        }
        // 路径需要 home 展开（~/x、$HOME、空串）
        Cow::Owned(s) => {
            let segments = split_path_into_segments(&s);
            let idx = segments.len().saturating_sub(2);
            segments
                .get(idx)
                .map(|seg| Cow::Owned(seg.full_path.to_owned()))
        }
    }
}

pub fn join_local_path(base: &str, name: &str) -> String {
    Path::new(base).join(name).to_string_lossy().to_string()
}

pub fn validate_local_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name is empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed == "." || trimmed == ".." {
        return Err("invalid name".to_string());
    }
    if trimmed.contains("..") {
        return Err("invalid name".to_string());
    }
    Ok(())
}

pub fn unique_copy_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "copy".to_string());
    let ext = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    for index in 1..=100 {
        let candidate = parent.join(format!("{stem} ({index}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} (copy){ext}"))
}

pub fn would_move_directory_into_itself(source: &Path, target: &Path) -> bool {
    let Ok(source) = source.canonicalize() else {
        return false;
    };
    let target = target
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .unwrap_or_else(|| target.to_path_buf());
    target.starts_with(source)
}
pub fn extract_drive_letter(path: &Path) -> Option<char> {
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            // 普通盘符：C:
            Prefix::Disk(d)
            // 扩展长度路径：\\?\C:
            | Prefix::VerbatimDisk(d) => Some(d as char),
            // UNC、DeviceNS 等没有盘符
            _ => None,
        },
        _ => None,
    }
}

use memchr::memchr;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub name: &'a str,
    pub full_path: &'a str,
    pub root_is_drive: bool,
}

#[derive(Clone, Copy)]
struct SegRange {
    name_start: u32,
    full_end: u32,
    flags: u32,
}

const FLAG_ROOT_IS_DRIVE: u32 = 1;

#[derive(Clone)]
pub struct SegmentedPath<'a> {
    buf: Cow<'a, str>,
    segments: Vec<SegRange>,
}

impl<'a> SegmentedPath<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        self.buf.as_ref()
    }

    #[inline]
    pub fn as_borrowed(&self) -> Option<&'a str> {
        match &self.buf {
            Cow::Borrowed(s) => Some(*s),
            _ => None,
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> Option<Segment<'_>> {
        let r = self.segments.get(i)?;
        let buf = self.buf.as_ref();
        let full_end = r.full_end as usize;
        Some(Segment {
            name: &buf[r.name_start as usize..full_end],
            full_path: &buf[..full_end],
            root_is_drive: (r.flags & FLAG_ROOT_IS_DRIVE) != 0,
        })
    }

    #[inline]
    pub fn get_borrowed(&self, i: usize) -> Option<Segment<'a>> {
        let s = self.as_borrowed()?;
        let r = self.segments.get(i)?;
        let full_end = r.full_end as usize;
        Some(Segment {
            name: &s[r.name_start as usize..full_end],
            full_path: &s[..full_end],
            root_is_drive: (r.flags & FLAG_ROOT_IS_DRIVE) != 0,
        })
    }

    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            buf: self.buf.as_ref(),
            ranges: &self.segments,
            pos: 0,
        }
    }
}

impl<'a, 'b> IntoIterator for &'a SegmentedPath<'b> {
    type Item = Segment<'a>;
    type IntoIter = Iter<'a>;
    #[inline]
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> fmt::Debug for SegmentedPath<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

pub struct Iter<'a> {
    buf: &'a str,
    ranges: &'a [SegRange],
    pos: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = Segment<'a>;
    #[inline]
    fn next(&mut self) -> Option<Segment<'a>> {
        let r = self.ranges.get(self.pos)?;
        self.pos += 1;
        let full_end = r.full_end as usize;
        Some(Segment {
            name: &self.buf[r.name_start as usize..full_end],
            full_path: &self.buf[..full_end],
            root_is_drive: (r.flags & FLAG_ROOT_IS_DRIVE) != 0,
        })
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.ranges.len() - self.pos;
        (n, Some(n))
    }
}

impl<'a> ExactSizeIterator for Iter<'a> {}

pub fn normalize_path_separators<'a>(
    path: Cow<'a, str>,
    from_sep: char,
    to_sep: char,
) -> Cow<'a, str> {
  let mut last = 0;
    let mut out: Option<String> = None;
    for (i, _) in path.match_indices(from_sep) {
        let buf = out.get_or_insert_with(|| String::with_capacity(path.len()));
        buf.push_str(&path[last..i]); 
        buf.push(to_sep);
        last = i + 1;
    }
    match out {
        Some(mut buf) => {
            buf.push_str(&path[last..]);
            Cow::Owned(buf)
        }
        None => path,
    }
}

pub fn split_path_into_segments<'a>(path: impl Into<Cow<'a, str>>) -> SegmentedPath<'a> {
    let cow = path.into();
    let bytes = cow.as_bytes();
    let mut segments = Vec::with_capacity(8);

    if bytes.starts_with(b"//") {
       split_unc(bytes, &mut segments, b'/');
    } else if bytes.starts_with(b"\\\\") {
        split_unc(bytes, &mut segments, b'\\');
    } else if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let root_end = if bytes.len() >= 3 && bytes[2] == b'\\' {
            3
        } else {
            2
        };
        segments.push(SegRange {
            name_start: 0,
            full_end: root_end as u32,
            flags: FLAG_ROOT_IS_DRIVE,
        });
        if root_end < bytes.len() {
            push_body(&bytes[root_end..], root_end, &mut segments, b'\\');
        }
    } else {
        segments.push(SegRange {
            name_start: 0,
            full_end: 1,
            flags: 0,
        });
        if bytes.len() > 1 {
            push_body(&bytes[1..], 1, &mut segments, b'/');
        }
    }

    SegmentedPath { buf: cow, segments }
}

fn split_unc(bytes: &[u8], segments: &mut Vec<SegRange>, sep: u8) {
    let rest = &bytes[2..];
    let server_end = memchr(sep, rest).unwrap_or(rest.len());

    if server_end == 0 {
 
        segments.push(SegRange { name_start: 0, full_end: 1, flags: 0 });
        if bytes.len() > 1 {
            push_body(&bytes[1..], 1, segments, sep);
        }
        return;
    }

    let after_server = 2 + server_end;
    if after_server >= bytes.len() {
   
        segments.push(SegRange {
            name_start: 0,
            full_end: bytes.len() as u32,
            flags: 0,
        });
        return;
    }


    let share_start = after_server + 1;
    let share_end = memchr(sep, &bytes[share_start..])
        .map(|i| share_start + i)
        .unwrap_or(bytes.len());


    let root_end = if share_end < bytes.len() { share_end + 1 } else { share_end };
    segments.push(SegRange {
        name_start: 0,
        full_end: root_end as u32,
        flags: 0,
    });

    if root_end < bytes.len() {
        push_body(&bytes[root_end..], root_end, segments, sep);
    }
}

#[inline]
fn push_body(bytes: &[u8], base: usize, segments: &mut Vec<SegRange>, seperator: u8) {
    let mut start = 0;
    let mut pos = 0;
    while let Some(off) = memchr(seperator, &bytes[pos..]) {
        let i = pos + off;
        if i > start {
            segments.push(SegRange {
                name_start: (base + start) as u32,
                full_end: (base + i) as u32,
                flags: 0,
            });
        }
        pos = i + 1;
        start = pos;
    }
    if start < bytes.len() {
        segments.push(SegRange {
            name_start: (base + start) as u32,
            full_end: (base + bytes.len()) as u32,
            flags: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_download_path_uses_downloads_when_available_and_temp_otherwise() {
        for (available, expected) in [(true, "/home/alice/Downloads"), (false, "/tmp")] {
            let downloads = PathBuf::from("/home/alice/Downloads");
            let selected = select_default_download_path(
                Some(downloads.clone()),
                PathBuf::from("/tmp"),
                |path| available && path == downloads,
            );
            assert_eq!(selected, PathBuf::from(expected), "available={available}");
        }
    }

    #[test]
    fn sidebar_locations_keep_only_available_directories_in_semantic_order() {
        let existing = [
            PathBuf::from("/home/alice"),
            PathBuf::from("/Applications"),
            PathBuf::from("/home/alice/Documents"),
            PathBuf::from("/home/alice/Downloads"),
        ];

        let locations = sidebar_locations_from_candidates(
            [
                (
                    LocalSidebarLocationKind::Home,
                    Some(PathBuf::from("/home/alice")),
                ),
                (
                    LocalSidebarLocationKind::Applications,
                    Some(PathBuf::from("/Applications")),
                ),
                (
                    LocalSidebarLocationKind::Desktop,
                    Some(PathBuf::from("/home/alice/Desktop")),
                ),
                (
                    LocalSidebarLocationKind::Documents,
                    Some(PathBuf::from("/home/alice/Documents")),
                ),
                (
                    LocalSidebarLocationKind::Downloads,
                    Some(PathBuf::from("/home/alice/Downloads")),
                ),
            ],
            |path| existing.iter().any(|candidate| candidate == path),
        );

        assert_eq!(
            locations
                .iter()
                .map(|location| location.kind)
                .collect::<Vec<_>>(),
            vec![
                LocalSidebarLocationKind::Home,
                LocalSidebarLocationKind::Applications,
                LocalSidebarLocationKind::Documents,
                LocalSidebarLocationKind::Downloads,
            ]
        );
    }

    #[test]
    fn sidebar_locations_preserve_redirected_system_paths() {
        let redirected_desktop = PathBuf::from(r"C:\Users\alice\OneDrive\Desktop");

        let locations = sidebar_locations_from_candidates(
            [(
                LocalSidebarLocationKind::Desktop,
                Some(redirected_desktop.clone()),
            )],
            |_| true,
        );

        assert_eq!(locations[0].path, redirected_desktop.to_string_lossy());
    }

    #[test]
    fn windows_drive_parent_never_becomes_a_unix_style_path() {
        assert_eq!(
            local_parent_path(r"D:\Projects\oxide").as_deref(),
            Some(r"D:\Projects")
        );
        assert_eq!(local_parent_path(r"D:\Projects").as_deref(), Some(r"D:\"));
        assert_eq!(local_parent_path(r"D:\"), None);
    }

    #[test]
    fn validate_local_name_rejects_traversal_and_separators() {
        assert!(validate_local_name("notes.txt").is_ok());
        assert!(validate_local_name("../notes.txt").is_err());
        assert!(validate_local_name("folder/notes.txt").is_err());
        assert!(validate_local_name("..").is_err());
    }
}
