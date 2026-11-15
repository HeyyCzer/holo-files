//! `.gitignore`-style ignore rules applied in both directions (no placeholders are
//! created for ignored remote items, and ignored local items are never uploaded).

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{naming, path::RelPath};

/// Per-root ignore file name.
pub const IGNORE_FILE: &str = ".holoignore";

/// Development junk ignored by default.
pub const DEFAULT_PATTERNS: &[&str] = &[
    "node_modules/",
    ".git/",
    "dist/",
    "build/",
    "target/",
    ".next/",
    ".turbo/",
    ".cache/",
    ".venv/",
    "__pycache__/",
    "*.tmp",
    "~$*",
    "Thumbs.db",
    "desktop.ini",
];

#[derive(Debug, Clone)]
pub struct IgnoreRules {
    matcher: Gitignore,
}

impl IgnoreRules {
    /// Build from defaults + extra patterns + `<root>/.holoignore` (if present).
    pub fn load(root: &Path, use_defaults: bool, extra: &[String]) -> Result<Self, String> {
        let mut b = GitignoreBuilder::new(root);
        if use_defaults {
            for p in DEFAULT_PATTERNS {
                b.add_line(None, p).map_err(|e| e.to_string())?;
            }
        }
        for p in extra {
            b.add_line(None, p).map_err(|e| e.to_string())?;
        }
        let file = root.join(IGNORE_FILE);
        if file.exists()
            && let Some(e) = b.add(&file)
        {
            return Err(e.to_string());
        }
        Ok(Self {
            matcher: b.build().map_err(|e| e.to_string())?,
        })
    }

    pub fn from_patterns(patterns: &[&str]) -> Self {
        let mut b = GitignoreBuilder::new("");
        for p in patterns {
            let _ = b.add_line(None, p);
        }
        Self {
            matcher: b.build().expect("static patterns"),
        }
    }

    pub fn none() -> Self {
        Self {
            matcher: Gitignore::empty(),
        }
    }

    /// Whether `path` (or any parent folder) is ignored. Transient editor files always are.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        if path.is_root() {
            return false;
        }
        if !is_dir && naming::is_transient_name(path.name()) {
            return true;
        }
        self.matcher
            .matched_path_or_any_parents(Path::new(path.as_str()), is_dir)
            .is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_ignore_dev_junk_and_children() {
        let r = IgnoreRules::from_patterns(DEFAULT_PATTERNS);
        let p = |s| RelPath::parse(s).unwrap();
        assert!(r.is_ignored(&p("web/node_modules"), true));
        assert!(r.is_ignored(&p("web/node_modules/react/index.js"), false));
        assert!(r.is_ignored(&p("repo/.git/HEAD"), false));
        assert!(r.is_ignored(&p("~$report.docx"), false));
        assert!(!r.is_ignored(&p("docs/build.md"), false));
        assert!(!r.is_ignored(&p("src/main.rs"), false));
    }
}
