//! Iterative preflight before the recursive KDL parser sees theme input.

use std::{borrow::Cow, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

use super::ThemeError;

pub(super) const FILE_BYTES: usize = 1024 * 1024;
pub(super) const TOTAL_BYTES: usize = 4 * FILE_BYTES;
pub(super) const MAX_IMPORTS: usize = 64;
pub(super) const IMPORT_DEPTH: usize = 16;
pub(super) const MAX_NODES: usize = 10_000;
const NESTING: usize = 32;

pub(super) fn read(path: &Path) -> Result<String, ThemeError> {
    let read = || -> std::io::Result<String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "theme input must be a regular file",
            ));
        }
        if metadata.len() > FILE_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "theme file exceeds 1 MiB",
            ));
        }
        let mut bytes = Vec::new();
        file.take(FILE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > FILE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "theme file exceeds 1 MiB",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    };
    read().map_err(|source| ThemeError::Import {
        path: path.display().to_string(),
        source,
    })
}

fn limit(reason: &'static str) -> ThemeError {
    ThemeError::InputLimit(reason)
}
fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Returns equivalent input with only block-comment bodies normalized, and a
/// conservative node count that includes slashdashed nodes. Byte positions and
/// line breaks stay unchanged. KDL remains responsible for syntax validation.
pub(super) fn preflight(src: &str) -> Result<(Cow<'_, str>, usize), ThemeError> {
    if src.len() > FILE_BYTES {
        return Err(limit("file exceeds 1 MiB"));
    }
    let bytes = src.as_bytes();
    let mut normalized: Option<Vec<u8>> = None;
    let (mut i, mut blocks, mut slashdashes, mut annotations, mut nodes) =
        (0, 0usize, 0usize, 0usize, 0usize);
    let mut node_start = true;
    let mut continuation = false;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            let end = comment_end(bytes, i)?;
            let output = normalized.get_or_insert_with(|| bytes.to_vec());
            // KDL 6's block-comment parser also recurses on each isolated '*'
            // or '/'. Flatten ignored inner delimiters after validating them.
            for byte in &mut output[i + 2..end - 2] {
                if matches!(*byte, b'*' | b'/') {
                    *byte = b' ';
                }
            }
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() {
                let c = src[i..].chars().next().expect("in bounds");
                if is_newline(c) {
                    break;
                }
                i += c.len_utf8();
            }
            continue;
        }
        if bytes[i..].starts_with(b"/-") {
            slashdashes += 1;
            if slashdashes > NESTING {
                return Err(limit("slashdash nesting exceeds 32"));
            }
            i += 2;
            continue;
        }
        let c = src[i..].chars().next().expect("in bounds");
        if is_newline(c) {
            if !continuation {
                node_start = true;
            }
            continuation = false;
            i += c.len_utf8();
            // CRLF is one newline, including after a continuation.
            if c == '\r' && bytes.get(i) == Some(&b'\n') {
                i += 1;
            }
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += c.len_utf8();
            continue;
        }
        match c {
            '\\' => {
                continuation = true;
                i += 1;
                continue;
            }
            '{' => {
                blocks += 1;
                if blocks > NESTING {
                    return Err(limit("child-block nesting exceeds 32"));
                }
                node_start = true;
                slashdashes = 0;
                i += 1;
                continue;
            }
            '}' => {
                blocks = blocks.saturating_sub(1);
                node_start = false;
                slashdashes = 0;
                i += 1;
                continue;
            }
            ';' => {
                node_start = true;
                slashdashes = 0;
                i += 1;
                continue;
            }
            '(' => {
                annotations += 1;
                if annotations > NESTING {
                    return Err(limit("annotation nesting exceeds 32"));
                }
                i += 1;
                continue;
            }
            ')' => {
                annotations = annotations.saturating_sub(1);
                i += 1;
                continue;
            }
            '=' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        if node_start && annotations == 0 {
            nodes += 1;
            if nodes > MAX_NODES {
                return Err(limit("node count exceeds 10000"));
            }
            node_start = false;
        }
        slashdashes = 0;
        let hashes = bytes[i..].iter().take_while(|&&b| b == b'#').count();
        if c == '"' || (hashes > 0 && bytes.get(i + hashes) == Some(&b'"')) {
            i = string_end(bytes, i + hashes, hashes)?;
        } else {
            i += c.len_utf8();
            while i < bytes.len() {
                let c = src[i..].chars().next().expect("in bounds");
                if c.is_whitespace()
                    || matches!(c, '{' | '}' | '(' | ')' | '=' | ';' | '"' | '\\')
                    || bytes[i..].starts_with(b"/*")
                    || bytes[i..].starts_with(b"//")
                    || bytes[i..].starts_with(b"/-")
                {
                    break;
                }
                i += c.len_utf8();
            }
        }
    }
    let text = match normalized {
        Some(bytes) => {
            Cow::Owned(String::from_utf8(bytes).expect("only ASCII comment bytes changed"))
        }
        None => Cow::Borrowed(src),
    };
    Ok((text, nodes))
}

fn comment_end(bytes: &[u8], start: usize) -> Result<usize, ThemeError> {
    let (mut i, mut depth) = (start + 2, 1usize);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            depth += 1;
            if depth > NESTING {
                return Err(limit("block-comment nesting exceeds 32"));
            }
            i += 2;
        } else if bytes[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Ok(i);
            }
        } else {
            i += 1;
        }
    }
    Err(ThemeError::Kdl("unterminated block comment".into()))
}

fn string_end(bytes: &[u8], quote: usize, hashes: usize) -> Result<usize, ThemeError> {
    let quotes = if bytes[quote..].starts_with(b"\"\"\"") {
        3
    } else {
        1
    };
    let mut i = quote + quotes;
    while i < bytes.len() {
        if hashes == 0 && bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i..].starts_with(&b"\"\"\""[..quotes]) {
            let end = i + quotes;
            if bytes
                .get(end..end + hashes)
                .is_some_and(|suffix| suffix.iter().all(|&b| b == b'#'))
            {
                return Ok(end + hashes);
            }
        }
        i += 1;
    }
    Err(ThemeError::Kdl("unterminated string".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kdl::KdlDocument;

    fn equivalent(src: &str) {
        let (safe, _) = preflight(src).unwrap();
        assert_eq!(safe.len(), src.len());
        assert_eq!(
            safe.match_indices('\n').map(|(i, _)| i).collect::<Vec<_>>(),
            src.match_indices('\n').map(|(i, _)| i).collect::<Vec<_>>()
        );
        let mut original: KdlDocument = src.parse().unwrap();
        let mut guarded: KdlDocument = safe.parse().unwrap();
        original.clear_format_recursive();
        guarded.clear_format_recursive();
        // Spans reflect formatting, so compare canonical documents.
        assert_eq!(original.to_string(), guarded.to_string());
    }

    #[test]
    fn lexical_guard_preserves_kdl_strings_comments_and_continuations() {
        for src in [
            "node \"{ /* /- \\\" }\"\n",
            "node #\"{ /* /- \\\" }\"#\n",
            "node ##\"literal \"# { /* /-\"##\n",
            "node \"\"\"\n  { /* /- }\n  \"\"\"\n",
            "node #\"\"\"\n  { /* /- }\n  \"\"\"#\n",
            "/* outer /* inner */ trailing */ node /* before entry */ 1\n",
            "node \\\n  1 /* nested /* x */ y */\nnext\n",
            "node \\\r\n  1\r\nnext\n",
            "// { /* /-\nnode\u{2028}next\n",
            "/- node { child; }\nnode /- 1 2\n",
            "(tag)node (tag)\"value\" key = (tag)2\n",
        ] {
            equivalent(src);
        }
    }

    #[test]
    fn lexical_guard_checks_comment_delimiters_before_normalizing() {
        equivalent("/* /**/ /***/ /* * / ** // */ */ node\n");
        for src in ["/* missing", "/* outer /* inner */", "/*/*/"] {
            assert!(preflight(src).is_err(), "{src}");
        }
        let source = format!("/*{}*/ node\n", "* / ".repeat(50_000));
        let (safe, _) = preflight(&source).unwrap();
        assert_eq!(safe.len(), source.len());
        assert_eq!(safe.parse::<KdlDocument>().unwrap().nodes().len(), 1);
    }

    #[test]
    fn lexical_guard_bounds_recursive_kdl_constructs() {
        for depth in [32, 33] {
            let children = format!("{}node{}", "node {".repeat(depth), "}".repeat(depth));
            let comments = format!("{}x{} node\n", "/*".repeat(depth), "*/".repeat(depth));
            let slashdash = format!("node {}1 {}2\n", "/- ".repeat(depth), "1 ".repeat(depth));
            for src in [&children, &comments, &slashdash] {
                let safe = preflight(src);
                assert_eq!(safe.is_ok(), depth == 32, "{src}");
                if let Ok((safe, _)) = safe {
                    assert!(safe.parse::<KdlDocument>().is_ok(), "{src}");
                }
            }
        }
        let quoted = format!("node #\"{}\"#\n", "{/* /-".repeat(100));
        assert!(preflight(&quoted).is_ok());
    }

    #[test]
    fn lexical_guard_node_budget_includes_ignored_nodes() {
        assert_eq!(
            preflight(&"/- node\n".repeat(MAX_NODES)).unwrap().1,
            MAX_NODES
        );
        assert!(preflight(&"/- node\n".repeat(MAX_NODES + 1)).is_err());
        assert_eq!(preflight("(tag)node \\\n 1\nnode\n").unwrap().1, 2);
    }

    #[test]
    fn theme_file_read_rejects_special_and_oversize_files() {
        let dir = std::env::temp_dir().join(format!("awob-theme-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("scene.kdl");
        std::fs::write(&file, "scene {}").unwrap();
        let link = dir.join("linked.kdl");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read(&link).unwrap(), "scene {}");
        assert!(read(&dir).is_err());
        assert!(read(Path::new("/dev/zero")).is_err());
        let fifo = dir.join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR,
            0,
        )
        .unwrap();
        assert!(read(&fifo).is_err());
        std::fs::write(&file, vec![b' '; FILE_BYTES + 1]).unwrap();
        assert!(read(&file).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
