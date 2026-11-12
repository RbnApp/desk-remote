//! Receiver-side file name rules.

use std::path::{Path, PathBuf};

/// Windows device names that cannot be used as file names (with or without extension).
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest accepted name in characters (Windows allows 255 per path component).
const MAX_NAME_CHARS: usize = 200;

/// True when `name` is a plain base file name that is safe to create in the receive folder.
///
/// Rejected: empty, `.`/`..`, path separators, drive letters and NTFS alternate streams
/// (`:`), wildcard and control characters, trailing dots or spaces and reserved device names.
/// A well-behaved sender only ever offers a base name, so anything else is refused outright
/// instead of being rewritten.
#[must_use]
pub fn is_safe_file_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return false;
    }
    if name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

/// First free path for `name` inside `dir`: `name`, then `stem (1).ext`, `stem (2).ext`, ...
#[must_use]
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_pass() {
        for ok in [
            "report.pdf",
            "a b (2).txt",
            "arşiv.zip",
            "noext",
            ".gitignore",
            "x.tar.gz",
        ] {
            assert!(is_safe_file_name(ok), "{ok}");
        }
    }

    #[test]
    fn hostile_names_are_refused() {
        let bad = [
            "",
            ".",
            "..",
            r"..\..\x.txt",
            "../x.txt",
            "dir/x.txt",
            r"C:\x.txt",
            "a:b",
            "x.txt:stream",
            "CON",
            "con.txt",
            "NUL",
            "Lpt1.log",
            "COM9.tar",
            "bad|name",
            "what?.txt",
            "star*.txt",
            "tab\there",
            "ends.with.dot.",
            "ends with space ",
        ];
        for name in bad {
            assert!(!is_safe_file_name(name), "{name:?} must be refused");
        }
        assert!(!is_safe_file_name(&"a".repeat(MAX_NAME_CHARS + 1)));
        // Not reserved: only the stem before the first dot counts, and COM10 is a normal name.
        assert!(is_safe_file_name("COM10.txt"));
        assert!(is_safe_file_name("console.txt"));
    }

    #[test]
    fn clashes_get_a_numbered_name() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        assert_eq!(unique_path(dir.path(), "a.txt"), dir.path().join("a.txt"));
        std::fs::write(dir.path().join("a.txt"), b"1")?;
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (1).txt")
        );
        std::fs::write(dir.path().join("a (1).txt"), b"2")?;
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (2).txt")
        );
        std::fs::write(dir.path().join("noext"), b"3")?;
        assert_eq!(
            unique_path(dir.path(), "noext"),
            dir.path().join("noext (1)")
        );
        Ok(())
    }
}
