use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Path relative to a sync root, `/`-separated, no leading/trailing slash.
/// The empty path is the root itself.
///
/// Windows is case-insensitive, so lookups must go through [`RelPath::key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct RelPath(String);

impl RelPath {
    pub fn root() -> Self {
        Self(String::new())
    }

    /// Normalise any `/` or `\` separated relative path. Rejects `..` and absolute paths.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = Vec::new();
        for part in s.split(['/', '\\']) {
            match part {
                "" | "." => continue,
                ".." => return None,
                p if p.contains(':') && parts.is_empty() && p.len() == 2 => return None,
                p => parts.push(p),
            }
        }
        Some(Self(parts.join("/")))
    }

    /// Relative path of `path` below `root`, or `None` if it is outside.
    pub fn from_local(root: &Path, path: &Path) -> Option<Self> {
        let rel = strip_prefix_ci(path, root)?;
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(p) => parts.push(p.to_str()?.to_owned()),
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Case-insensitive comparison key.
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    pub fn join(&self, name: &str) -> Self {
        if self.0.is_empty() {
            Self(name.to_owned())
        } else {
            Self(format!("{}/{}", self.0, name))
        }
    }

    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => Self(self.0[..i].to_owned()),
            None => Self::root(),
        })
    }

    pub fn name(&self) -> &str {
        match self.0.rfind('/') {
            Some(i) => &self.0[i + 1..],
            None => &self.0,
        }
    }

    pub fn depth(&self) -> usize {
        if self.0.is_empty() {
            0
        } else {
            self.0.matches('/').count() + 1
        }
    }

    /// Whether `self` equals `other` or lies below it (case-insensitive).
    pub fn starts_with(&self, other: &RelPath) -> bool {
        if other.is_root() {
            return true;
        }
        let (a, b) = (self.key(), other.key());
        a == b || (a.starts_with(&b) && a.as_bytes().get(b.len()) == Some(&b'/'))
    }

    /// Rebase `self` from `old_prefix` to `new_prefix` (used when a folder moves).
    pub fn rebase(&self, old_prefix: &RelPath, new_prefix: &RelPath) -> Option<Self> {
        if !self.starts_with(old_prefix) {
            return None;
        }
        let rest = &self.0[old_prefix.0.len()..];
        let rest = rest.trim_start_matches('/');
        Some(if rest.is_empty() {
            new_prefix.clone()
        } else {
            new_prefix.join(rest)
        })
    }

    pub fn to_local(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for part in self.0.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("/")
        } else {
            f.write_str(&self.0)
        }
    }
}

fn strip_prefix_ci<'a>(path: &'a Path, root: &Path) -> Option<&'a Path> {
    if let Ok(rest) = path.strip_prefix(root) {
        return Some(rest);
    }
    // Case-insensitive fallback (drive letters and folder names on NTFS).
    let mut pc = path.components();
    for rc in root.components() {
        let p = pc.next()?;
        if !p.as_os_str().eq_ignore_ascii_case(rc.as_os_str())
            && p.as_os_str().to_str()?.to_lowercase() != rc.as_os_str().to_str()?.to_lowercase()
        {
            return None;
        }
    }
    Some(pc.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalises_separators() {
        assert_eq!(RelPath::parse(r"\a\\b/c/").unwrap().as_str(), "a/b/c");
        assert!(RelPath::parse("a/../b").is_none());
        assert!(RelPath::parse("").unwrap().is_root());
    }

    #[test]
    fn parent_name_depth() {
        let p = RelPath::parse("a/b/c.txt").unwrap();
        assert_eq!(p.name(), "c.txt");
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(p.depth(), 3);
        assert_eq!(
            RelPath::parse("x").unwrap().parent().unwrap(),
            RelPath::root()
        );
        assert!(RelPath::root().parent().is_none());
    }

    #[test]
    fn starts_with_is_component_aware_and_case_insensitive() {
        let p = RelPath::parse("Docs/Report.docx").unwrap();
        assert!(p.starts_with(&RelPath::parse("docs").unwrap()));
        assert!(
            !RelPath::parse("Docs2/x")
                .unwrap()
                .starts_with(&RelPath::parse("Docs").unwrap())
        );
    }

    #[test]
    fn rebase_moves_subtree() {
        let p = RelPath::parse("a/b/c").unwrap();
        let r = p
            .rebase(
                &RelPath::parse("a/b").unwrap(),
                &RelPath::parse("z").unwrap(),
            )
            .unwrap();
        assert_eq!(r.as_str(), "z/c");
    }

    #[test]
    fn from_local_roundtrip() {
        let root = Path::new(r"C:\Sync\Root");
        let p = RelPath::from_local(root, Path::new(r"c:\sync\root\A\b.txt")).unwrap();
        assert_eq!(p.as_str(), "A/b.txt");
        assert_eq!(p.to_local(root), PathBuf::from(r"C:\Sync\Root\A\b.txt"));
        assert!(RelPath::from_local(root, Path::new(r"C:\Other\x")).is_none());
    }
}
