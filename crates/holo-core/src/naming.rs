//! Windows / cloud naming rules and conflict-copy naming.

use chrono::{DateTime, Local};

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameIssue {
    #[error("empty name")]
    Empty,
    #[error("contains forbidden character '{0}'")]
    ForbiddenChar(char),
    #[error("reserved device name")]
    Reserved,
    #[error("ends with a dot or space")]
    TrailingDotOrSpace,
    #[error("contains control characters")]
    Control,
}

/// Validate a single path component for Windows (and therefore OneDrive).
pub fn validate_name(name: &str) -> Result<(), NameIssue> {
    if name.is_empty() {
        return Err(NameIssue::Empty);
    }
    if let Some(c) = name.chars().find(|c| FORBIDDEN_CHARS.contains(c)) {
        return Err(NameIssue::ForbiddenChar(c));
    }
    if name.chars().any(|c| (c as u32) < 0x20) {
        return Err(NameIssue::Control);
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(NameIssue::TrailingDotOrSpace);
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(NameIssue::Reserved);
    }
    Ok(())
}

/// Make a remote name usable locally by replacing forbidden characters with
/// visually similar full-width ones. Returns the name unchanged if already valid.
pub fn sanitize_for_windows(name: &str) -> String {
    if validate_name(name).is_ok() {
        return name.to_owned();
    }
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' => '＜',
            '>' => '＞',
            ':' => '：',
            '"' => '＂',
            '/' => '／',
            '\\' => '＼',
            '|' => '｜',
            '?' => '？',
            '*' => '＊',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
        out.push('_');
    }
    if out.is_empty() {
        out.push('_');
    }
    if validate_name(&out) == Err(NameIssue::Reserved) {
        out.insert(0, '_');
    }
    out
}

/// `report.docx` -> `report (conflict HOSTNAME 2026-11-15 1634).docx`.
pub fn conflict_name(name: &str, machine: &str, at: DateTime<Local>) -> String {
    let tag = format!("conflict {} {}", machine, at.format("%Y-%m-%d %H%M%S"));
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({tag}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({tag})"),
    }
}

/// Files that are transient by nature (editor/Office temp files) and should never be
/// uploaded even when not explicitly ignored.
pub fn is_transient_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("~$")
        || lower.starts_with(".~lock.")
        || lower.ends_with(".tmp")
        || lower.ends_with(".crdownload")
        || lower.ends_with(".partial")
        || lower.ends_with(".holo-tmp")
        || lower == "thumbs.db"
        || lower == "desktop.ini"
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn validates_windows_rules() {
        assert!(validate_name("ok.txt").is_ok());
        assert_eq!(validate_name("a:b"), Err(NameIssue::ForbiddenChar(':')));
        assert_eq!(validate_name("CON.txt"), Err(NameIssue::Reserved));
        assert_eq!(validate_name("trail."), Err(NameIssue::TrailingDotOrSpace));
        assert!(validate_name("CONSOLE.txt").is_ok());
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize_for_windows("a:b?.txt"), "a：b？.txt");
        assert_eq!(sanitize_for_windows("nul"), "_nul");
        assert_eq!(sanitize_for_windows("x."), "x_");
    }

    #[test]
    fn conflict_names_keep_extension() {
        let at = Local.with_ymd_and_hms(2026, 11, 15, 16, 34, 5).unwrap();
        assert_eq!(
            conflict_name("r.docx", "PC", at),
            "r (conflict PC 2026-11-15 163405).docx"
        );
        assert_eq!(
            conflict_name(".env", "PC", at),
            ".env (conflict PC 2026-11-15 163405)"
        );
        assert_eq!(
            conflict_name("Makefile", "PC", at),
            "Makefile (conflict PC 2026-11-15 163405)"
        );
    }
}
