//! `.gitignore`-style ignore rules applied in both directions (no placeholders are
//! created for ignored remote items, and ignored local items are never uploaded).
//!
//! Rules come from the built-in defaults, the config's extra patterns, and any
//! `.holoignore` file in the root or in a subfolder. Like `.gitignore`, a nested file's
//! patterns are relative to its own folder, and deeper files take precedence.

use std::{collections::BTreeMap, path::Path};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{naming, path::RelPath};

/// Ignore file name, read from the root and from any subfolder.
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
    /// Defaults + config patterns, relative to the root.
    base: Gitignore,
    /// `.holoignore` files by folder key (`RelPath::key`; "" is the root).
    files: BTreeMap<String, (RelPath, Gitignore)>,
}

/// `./foo` means "relative to this folder" to most people; in gitignore syntax that is `/foo`.
fn normalize(line: &str) -> &str {
    let t = line.trim_start();
    if t.starts_with("./") { &t[1..] } else { line }
}

fn build<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<Gitignore, String> {
    let mut b = GitignoreBuilder::new("");
    for l in lines {
        b.add_line(None, normalize(l)).map_err(|e| e.to_string())?;
    }
    b.build().map_err(|e| e.to_string())
}

impl IgnoreRules {
    /// Build from defaults + extra patterns + `<root>/.holoignore` (if present).
    /// Nested files are added later with [`IgnoreRules::set_file`].
    pub fn load(root: &Path, use_defaults: bool, extra: &[String]) -> Result<Self, String> {
        let defaults: &[&str] = if use_defaults { DEFAULT_PATTERNS } else { &[] };
        let mut rules = Self {
            base: build(
                defaults
                    .iter()
                    .copied()
                    .chain(extra.iter().map(String::as_str)),
            )?,
            files: BTreeMap::new(),
        };
        let file = root.join(IGNORE_FILE);
        if file.exists() {
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            rules.set_file(&RelPath::root(), &text)?;
        }
        Ok(rules)
    }

    /// [`IgnoreRules::load`] plus every nested `.holoignore` under `root` (walks the
    /// tree, skipping ignored folders). Unreadable files are skipped.
    pub fn load_tree(root: &Path, use_defaults: bool, extra: &[String]) -> Result<Self, String> {
        let mut rules = Self::load(root, use_defaults, extra)?;
        let mut stack = vec![RelPath::root()];
        while let Some(dir) = stack.pop() {
            let local = dir.to_local(root);
            if !dir.is_root()
                && let Ok(text) = std::fs::read_to_string(local.join(IGNORE_FILE))
            {
                rules.set_file(&dir, &text)?;
            }
            let Ok(entries) = std::fs::read_dir(&local) else {
                continue;
            };
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir())
                    && let Some(name) = e.file_name().to_str()
                {
                    let child = dir.join(name);
                    if !rules.is_ignored(&child, true) {
                        stack.push(child);
                    }
                }
            }
        }
        Ok(rules)
    }

    pub fn from_patterns(patterns: &[&str]) -> Self {
        Self {
            base: build(patterns.iter().copied()).expect("static patterns"),
            files: BTreeMap::new(),
        }
    }

    pub fn none() -> Self {
        Self {
            base: Gitignore::empty(),
            files: BTreeMap::new(),
        }
    }

    /// Set (or replace) the rules of `<dir>/.holoignore`.
    pub fn set_file(&mut self, dir: &RelPath, contents: &str) -> Result<(), String> {
        let gi = build(contents.lines())?;
        self.files.insert(dir.key(), (dir.clone(), gi));
        Ok(())
    }

    /// Forget `<dir>/.holoignore`. Returns whether it was known.
    pub fn remove_file(&mut self, dir: &RelPath) -> bool {
        self.files.remove(&dir.key()).is_some()
    }

    /// Folders that have a `.holoignore` loaded.
    pub fn files(&self) -> impl Iterator<Item = &RelPath> {
        self.files.values().map(|(d, _)| d)
    }

    /// Whether `path` (or any parent folder) is ignored. Transient editor files always are.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        if path.is_root() {
            return false;
        }
        if !is_dir && naming::is_transient_name(path.name()) {
            return true;
        }
        // Deepest `.holoignore` first; the first one with an opinion wins.
        let mut dir = path.parent();
        while let Some(d) = dir {
            if let Some((_, gi)) = self.files.get(&d.key()) {
                let rel = path.as_str()[d.as_str().len()..].trim_start_matches('/');
                let m = gi.matched_path_or_any_parents(Path::new(rel), is_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    return false;
                }
            }
            dir = d.parent();
        }
        self.base
            .matched_path_or_any_parents(Path::new(path.as_str()), is_dir)
            .is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn defaults_ignore_dev_junk_and_children() {
        let r = IgnoreRules::from_patterns(DEFAULT_PATTERNS);
        assert!(r.is_ignored(&p("web/node_modules"), true));
        assert!(r.is_ignored(&p("web/node_modules/react/index.js"), false));
        assert!(r.is_ignored(&p("repo/.git/HEAD"), false));
        assert!(r.is_ignored(&p("~$report.docx"), false));
        assert!(!r.is_ignored(&p("docs/build.md"), false));
        assert!(!r.is_ignored(&p("src/main.rs"), false));
    }

    #[test]
    fn nested_file_is_relative_to_its_folder() {
        let mut r = IgnoreRules::none();
        r.set_file(&p("Games/Launcher"), "*.log\n/cache/\n")
            .unwrap();
        assert!(r.is_ignored(&p("Games/Launcher/launcher.log"), false));
        assert!(r.is_ignored(&p("Games/Launcher/sub/x.log"), false));
        assert!(r.is_ignored(&p("Games/Launcher/cache/a.bin"), false));
        assert!(!r.is_ignored(&p("Games/Launcher/sub/cache/a.bin"), false));
        assert!(!r.is_ignored(&p("Games/other.log"), false));
        assert!(!r.is_ignored(&p("Games/Launcher/settings.json"), false));

        assert!(r.remove_file(&p("Games/Launcher")));
        assert!(!r.is_ignored(&p("Games/Launcher/launcher.log"), false));
    }

    #[test]
    fn dot_slash_prefix_anchors_to_the_folder() {
        let mut r = IgnoreRules::none();
        r.set_file(&p("Launcher"), "./**/**.log\n./tmp/\n").unwrap();
        assert!(r.is_ignored(&p("Launcher/launcher.log"), false));
        assert!(r.is_ignored(&p("Launcher/a/b/c.log"), false));
        assert!(r.is_ignored(&p("Launcher/tmp/x"), false));
        assert!(!r.is_ignored(&p("Launcher/a/tmp/x"), false));
        assert!(!r.is_ignored(&p("launcher.log"), false));
    }

    #[test]
    fn deeper_file_overrides_root_and_base() {
        let mut r = IgnoreRules::from_patterns(&["*.log"]);
        r.set_file(&RelPath::root(), "*.bak\n").unwrap();
        r.set_file(&p("keep"), "!*.log\n").unwrap();
        assert!(r.is_ignored(&p("a.log"), false));
        assert!(r.is_ignored(&p("x/a.bak"), false));
        assert!(!r.is_ignored(&p("keep/a.log"), false));
    }
}
