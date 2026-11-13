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

/// Atomically claims the first free path for `name` inside `dir`: `name`, then
/// `stem (1).ext`, `stem (2).ext`, ... The file is created empty (`create_new`), so two
/// transfers can never pick the same name; the caller then moves its finished file onto the
/// claimed path, replacing only that empty placeholder.
pub fn claim_unique_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 0u32..10_000 {
        let candidate = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free file name",
    ))
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
    fn clashes_get_a_numbered_name_and_nothing_is_overwritten() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        let first = claim_unique_path(dir.path(), "a.txt")?;
        assert_eq!(first, dir.path().join("a.txt"));
        std::fs::write(&first, b"keep me")?;
        // The placeholder from the claim already blocks the name: no second claim can race it.
        assert_eq!(
            claim_unique_path(dir.path(), "a.txt")?,
            dir.path().join("a (1).txt")
        );
        assert_eq!(
            claim_unique_path(dir.path(), "a.txt")?,
            dir.path().join("a (2).txt")
        );
        assert_eq!(std::fs::read(&first)?, b"keep me");
        std::fs::write(dir.path().join("noext"), b"3")?;
        assert_eq!(
            claim_unique_path(dir.path(), "noext")?,
            dir.path().join("noext (1)")
        );
        Ok(())
    }
}
